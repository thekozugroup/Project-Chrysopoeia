//! What this ffmpeg can do, and which hardware encoders really work.
//!
//! `ffmpeg -encoders` only says what was compiled in. A hardware encoder is
//! only offered once a one-second test encode on the actual device succeeds,
//! so a missing driver or a GPU that wasn't passed into the container shows
//! up here, with a plain-language reason, instead of as failed jobs later.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrysopoeia_core::{
    AudioCodec, EncoderInfo, EncoderStatus, FfmpegInfo, GpuVendor, HwApi, VIDEO_ENCODERS,
};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::devices::{Devices, NodeAccess, Platform};
use crate::process::{RunError, run_capture};

/// Longest a single test encode may take before it is killed.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Test encodes running at the same time.
pub const TEST_CONCURRENCY: usize = 4;

/// Longest `ffmpeg -version`, `-encoders` or `-filters` may take.
const INFO_TIMEOUT: Duration = Duration::from_secs(10);

/// Render node used when a VA-API or QSV test has no device.
const DEFAULT_RENDER_NODE: &str = "/dev/dri/renderD128";

/// One second of black 256x256 video at 25 fps.
const TEST_SOURCE: &str = "color=c=black:s=256x256:r=25:d=1";

/// ffmpeg filters that verification and planning rely on.
pub const VERIFICATION_FILTERS: [&str; 5] =
    ["ssim", "psnr", "blackdetect", "freezedetect", "bwdif"];

/// Encoder names from `ffmpeg -encoders`, by media type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListedEncoders {
    pub video: BTreeSet<String>,
    pub audio: BTreeSet<String>,
}

/// Parse `ffmpeg -hide_banner -encoders`. Lines look like
/// ` V....D libx264              libx264 H.264 / AVC ...`; the legend lines
/// (` V..... = Video`) are skipped.
pub fn parse_encoders(text: &str) -> ListedEncoders {
    let mut listed = ListedEncoders::default();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(flags), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if flags.len() != 6
            || name == "="
            || !flags
                .bytes()
                .skip(1)
                .all(|b| b.is_ascii_uppercase() || b == b'.')
        {
            continue;
        }
        match flags.bytes().next() {
            Some(b'V') => {
                listed.video.insert(name.to_string());
            }
            Some(b'A') => {
                listed.audio.insert(name.to_string());
            }
            _ => {}
        }
    }
    listed
}

/// Parse `ffmpeg -hide_banner -filters`. Lines look like
/// ` TS. ssim              VV->V      Calculate the SSIM ...`.
pub fn parse_filters(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let flags = parts.next()?;
            let name = parts.next()?;
            let io = parts.next()?;
            (flags.len() == 3 && name != "=" && io.contains("->")).then(|| name.to_string())
        })
        .collect()
}

/// Audio encoders Chrysopoeia can target that this ffmpeg has, in
/// `AudioCodec::ALL` order.
pub fn available_audio_encoders(listed_audio: &BTreeSet<String>) -> Vec<String> {
    AudioCodec::ALL
        .iter()
        .filter(|c| **c != AudioCodec::Copy)
        .map(|c| c.ffmpeg_encoder())
        .filter(|name| listed_audio.contains(*name))
        .map(String::from)
        .collect()
}

/// The verification filters this ffmpeg has, in [`VERIFICATION_FILTERS`] order.
pub fn available_filters(listed: &BTreeSet<String>) -> Vec<String> {
    VERIFICATION_FILTERS
        .iter()
        .filter(|f| listed.contains(**f))
        .map(|f| (*f).to_string())
        .collect()
}

/// What running ffmpeg and ffprobe revealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegProbe {
    pub info: FfmpegInfo,
    pub encoders: ListedEncoders,
    pub filters: BTreeSet<String>,
}

/// First line of `-version` output when the tool runs successfully.
async fn tool_version(program: &Path) -> Option<String> {
    match run_capture(program, ["-version"], INFO_TIMEOUT).await {
        Ok(out) if out.status.success() => Some(
            out.stdout
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string(),
        ),
        Ok(out) => {
            tracing::warn!(program = %program.display(), stderr = %out.stderr.trim(), "tool exited with an error");
            None
        }
        Err(err) => {
            tracing::warn!(program = %program.display(), error = %err, "tool could not run");
            None
        }
    }
}

/// Stdout of `ffmpeg -hide_banner <flag>`, or empty on failure.
async fn ffmpeg_listing(ffmpeg: &Path, flag: &str) -> String {
    match run_capture(ffmpeg, ["-hide_banner", flag], INFO_TIMEOUT).await {
        Ok(out) if out.status.success() => out.stdout,
        Ok(out) => {
            tracing::warn!(flag, stderr = %out.stderr.trim(), "ffmpeg listing failed");
            String::new()
        }
        Err(err) => {
            tracing::warn!(flag, error = %err, "ffmpeg listing could not run");
            String::new()
        }
    }
}

/// Check that ffmpeg and ffprobe run, and list encoders and filters.
pub async fn probe_ffmpeg(ffmpeg: &Path, ffprobe: &Path) -> FfmpegProbe {
    let (version, probe_version) = tokio::join!(tool_version(ffmpeg), tool_version(ffprobe));
    let found = version.is_some();
    let (encoders, filters) = if found {
        let (enc, fil) = tokio::join!(
            ffmpeg_listing(ffmpeg, "-encoders"),
            ffmpeg_listing(ffmpeg, "-filters")
        );
        (parse_encoders(&enc), parse_filters(&fil))
    } else {
        (ListedEncoders::default(), BTreeSet::new())
    };
    FfmpegProbe {
        info: FfmpegInfo {
            ffmpeg_path: ffmpeg.display().to_string(),
            ffprobe_path: ffprobe.display().to_string(),
            found,
            ffprobe_found: probe_version.is_some(),
            version: version.filter(|v| !v.is_empty()),
        },
        encoders,
        filters,
    }
}

// ---------------------------------------------------------------------------
// Test encodes
// ---------------------------------------------------------------------------

/// ffmpeg arguments for a one-second test encode with `encoder` on `device`
/// (a render node for VA-API and Quick Sync; ignored otherwise). The hardware
/// setup matches what the worker uses for real jobs.
pub fn test_encode_args(encoder: &EncoderInfo, device: Option<&str>) -> Vec<String> {
    let node = device.unwrap_or(DEFAULT_RENDER_NODE);
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let mut push = |items: &[&str]| args.extend(items.iter().map(|s| (*s).to_string()));

    match encoder.api {
        HwApi::Vaapi => {
            let init = format!("vaapi=va:{node}");
            push(&["-init_hw_device", &init, "-filter_hw_device", "va"]);
        }
        HwApi::Qsv => {
            let init = format!("vaapi=va:{node}");
            push(&[
                "-init_hw_device",
                &init,
                "-init_hw_device",
                "qsv=qs@va",
                "-filter_hw_device",
                "qs",
            ]);
        }
        _ => {}
    }
    push(&["-f", "lavfi", "-i", TEST_SOURCE]);
    match encoder.api {
        HwApi::Vaapi => push(&["-vf", "format=nv12,hwupload"]),
        HwApi::Qsv => push(&["-vf", "format=nv12,hwupload=extra_hw_frames=64"]),
        HwApi::Rkmpp | HwApi::V4l2m2m => push(&["-pix_fmt", "nv12"]),
        HwApi::Nvenc | HwApi::Amf | HwApi::VideoToolbox | HwApi::Software => {
            push(&["-pix_fmt", "yuv420p"]);
        }
    }
    push(&["-c:v", encoder.name, "-frames:v", "25", "-f", "null", "-"]);
    args
}

/// Why an encoder isn't verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureKind {
    /// Not compiled into this ffmpeg.
    NotInBuild,
    /// Hardware checks were turned off.
    NotTested,
    /// No GPU that could run it (not tested).
    NoDevice,
    /// The GPU is on the PCI bus but has no driver loaded on the host.
    DriverNotLoaded,
    /// The render node exists in sysfs but not under `/dev`.
    DeviceMissing,
    /// libcuda / the NVIDIA driver isn't reachable.
    NvidiaDriverMissing,
    /// The NVIDIA driver is older than this ffmpeg needs.
    NvidiaDriverTooOld,
    /// All NVENC sessions are taken by other apps.
    NvencSessionLimit,
    /// VA-API could not initialise the device.
    VaapiUnavailable,
    /// The Intel media runtime (oneVPL / MFX) is missing.
    QsvRuntimeMissing,
    /// AMD's AMF runtime is missing.
    AmfRuntimeMissing,
    /// The Rockchip MPP device is missing.
    RkmppUnavailable,
    /// No V4L2 encoder device exists.
    NoEncoderDevice,
    /// The GPU works but can't encode this codec.
    CodecUnsupported,
    /// The device can't be opened by this process.
    Permission,
    /// The test encode hung.
    TimedOut,
    /// Anything else.
    Other,
}

impl FailureKind {
    /// Plain-language sentence for this failure.
    pub fn sentence(self, encoder: &EncoderInfo, device: Option<&str>) -> String {
        match self {
            Self::NotInBuild => "This ffmpeg build doesn't include it.".into(),
            Self::NotTested => "Not tested, because hardware checks are turned off.".into(),
            Self::NoDevice => match encoder.api {
                HwApi::Nvenc => "No NVIDIA GPU is visible to Chrysopoeia.".into(),
                HwApi::Qsv => "No Intel GPU is visible to Chrysopoeia.".into(),
                HwApi::Vaapi => "No Intel or AMD GPU is visible to Chrysopoeia.".into(),
                HwApi::Amf => "No AMD GPU is visible to Chrysopoeia.".into(),
                HwApi::VideoToolbox => "VideoToolbox is only available on a Mac.".into(),
                HwApi::Rkmpp => "Rockchip encoders only work on Rockchip ARM boards.".into(),
                HwApi::V4l2m2m => {
                    "V4L2 encoders only work on ARM boards such as the Raspberry Pi.".into()
                }
                HwApi::Software => "No device is needed.".into(),
            },
            Self::DriverNotLoaded => {
                "The GPU has no driver loaded on the host, so it can't be used.".into()
            }
            Self::DeviceMissing => match device {
                Some(d) => format!("{d} is missing, so the GPU isn't available to Chrysopoeia."),
                None => {
                    "The GPU's device file is missing, so it isn't available to Chrysopoeia.".into()
                }
            },
            Self::NvidiaDriverMissing => {
                "The NVIDIA driver isn't available inside the container.".into()
            }
            Self::NvidiaDriverTooOld => "The NVIDIA driver is too old for this ffmpeg.".into(),
            Self::NvencSessionLimit => {
                "The NVIDIA GPU has no free encoding sessions; other apps are using them all."
                    .into()
            }
            Self::VaapiUnavailable => match device {
                Some(d) => format!(
                    "The VA-API device {d} can't be used; the GPU's media driver may be missing."
                ),
                None => {
                    "The VA-API device can't be used; the GPU's media driver may be missing.".into()
                }
            },
            Self::QsvRuntimeMissing => "The Intel Quick Sync runtime isn't available.".into(),
            Self::AmfRuntimeMissing => {
                "AMD's AMF runtime isn't available (it needs AMD's proprietary driver).".into()
            }
            Self::RkmppUnavailable => {
                "The Rockchip video engine isn't available to Chrysopoeia.".into()
            }
            Self::NoEncoderDevice => "No hardware encoder device was found.".into(),
            Self::CodecUnsupported => {
                let codec = match encoder.codec {
                    chrysopoeia_core::VideoCodec::Hevc => "HEVC",
                    other => other.label(),
                };
                format!("This GPU can't encode {codec}.")
            }
            Self::Permission => match (encoder.api, device) {
                (HwApi::Vaapi | HwApi::Qsv, Some(d)) => {
                    format!("Chrysopoeia doesn't have permission to use {d}.")
                }
                (HwApi::Vaapi | HwApi::Qsv, None) => {
                    "Chrysopoeia doesn't have permission to use /dev/dri.".into()
                }
                _ => "Chrysopoeia doesn't have permission to use the GPU.".into(),
            },
            Self::TimedOut => "The test encode timed out.".into(),
            Self::Other => "The test encode failed.".into(),
        }
    }
}

/// Classify a failed test encode from ffmpeg's error output.
pub fn classify_failure(stderr: &str, timed_out: bool) -> FailureKind {
    if timed_out {
        return FailureKind::TimedOut;
    }
    let lower = stderr.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));

    if has(&[
        "driver does not support the required nvenc api version",
        "minimum required nvidia driver",
    ]) {
        return FailureKind::NvidiaDriverTooOld;
    }
    if has(&[
        "libcuda",
        "cuda_error_no_device",
        "no nvenc capable devices",
        "no cuda capable devices",
        "libnvidia-encode",
        "cannot init cuda",
        "cuinit(0) failed",
    ]) {
        return FailureKind::NvidiaDriverMissing;
    }
    if has(&["incompatible client key", "out of memory (10)"]) {
        return FailureKind::NvencSessionLimit;
    }
    // "Operation not permitted" is also ffmpeg's generic EPERM error code, so
    // it only counts when it is about a device file.
    let device_eperm = lower
        .lines()
        .any(|l| l.contains("operation not permitted") && l.contains("/dev/"));
    if has(&["permission denied"]) || device_eperm {
        return FailureKind::Permission;
    }
    if has(&[
        "mfx session",
        "mfxinit",
        "libmfx",
        "libvpl",
        "qsv=qs@va",
        "failed to create a qsv device",
        "error creating a qsv device",
    ]) {
        return FailureKind::QsvRuntimeMissing;
    }
    if has(&[
        "failed to initialise vaapi",
        "no va display",
        "vainitialize failed",
        "failed to create a vaapi device",
    ]) {
        return FailureKind::VaapiUnavailable;
    }
    if has(&[
        "libamfrt",
        "amf runtime",
        "failed to load amf",
        "amf failed to initialise",
    ]) {
        return FailureKind::AmfRuntimeMissing;
    }
    if has(&["mpp context", "mpp_service", "failed to init mpp"]) {
        return FailureKind::RkmppUnavailable;
    }
    if has(&["could not find a valid device"]) {
        return FailureKind::NoEncoderDevice;
    }
    if has(&[
        "no usable encoding profile",
        "not supported",
        "unsupported",
        "no capable devices found",
        "cannot create compression session",
    ]) {
        return FailureKind::CodecUnsupported;
    }
    FailureKind::Other
}

/// Strip the `[h264_nvenc @ 0x55d...] ` context prefix ffmpeg puts on lines.
fn strip_context(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix('[') {
        if let Some(end) = rest.find("] ") {
            if rest[..end].contains(" @ 0x") {
                return rest[end + 2..].trim_start();
            }
        }
    }
    line
}

/// The last `n` non-empty stderr lines, without ffmpeg's context prefixes,
/// each shortened to 200 characters, joined with " / ".
pub fn stderr_tail(stderr: &str, n: usize) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let start = lines.len().saturating_sub(n);
    lines[start..]
        .iter()
        .map(|l| {
            let l = strip_context(l);
            match l.char_indices().nth(200) {
                Some((cut, _)) => format!("{}...", &l[..cut]),
                None => l.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(" / ")
}

/// Sentence for a failure plus the ffmpeg error tail, for `EncoderStatus::error`.
pub fn describe_failure(
    kind: FailureKind,
    encoder: &EncoderInfo,
    device: Option<&str>,
    stderr: &str,
) -> String {
    let sentence = kind.sentence(encoder, device);
    let tail = stderr_tail(stderr, 3);
    if tail.is_empty() {
        sentence
    } else {
        format!("{sentence} Details: {tail}")
    }
}

/// A failed test encode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestFailure {
    pub timed_out: bool,
    pub stderr: String,
}

/// Run one test encode. `Ok` means ffmpeg exited successfully.
pub async fn run_test_encode(
    ffmpeg: &Path,
    args: &[String],
    timeout: Duration,
) -> Result<(), TestFailure> {
    match run_capture(ffmpeg, args, timeout).await {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(TestFailure {
            timed_out: false,
            stderr: out.stderr,
        }),
        Err(RunError::TimedOut) => Err(TestFailure {
            timed_out: true,
            stderr: String::new(),
        }),
        Err(err) => Err(TestFailure {
            timed_out: false,
            stderr: err.to_string(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Planning and running the checks
// ---------------------------------------------------------------------------

/// Result for one encoder, with the failure category hints are built from.
#[derive(Debug, Clone, PartialEq)]
pub struct EncoderCheck {
    pub status: EncoderStatus,
    /// Why it isn't verified; `None` when verified.
    pub failure: Option<FailureKind>,
}

impl EncoderCheck {
    fn verified(encoder: &EncoderInfo, device: Option<String>) -> Self {
        Self {
            status: EncoderStatus {
                name: encoder.name.to_string(),
                codec: encoder.codec,
                api: encoder.api,
                available: true,
                verified: true,
                device,
                error: None,
            },
            failure: None,
        }
    }

    fn failed(
        encoder: &EncoderInfo,
        available: bool,
        kind: FailureKind,
        device: Option<String>,
        error: String,
    ) -> Self {
        Self {
            status: EncoderStatus {
                name: encoder.name.to_string(),
                codec: encoder.codec,
                api: encoder.api,
                available,
                verified: false,
                device,
                error: Some(error),
            },
            failure: Some(kind),
        }
    }

    fn known_failure(
        encoder: &EncoderInfo,
        available: bool,
        kind: FailureKind,
        device: Option<String>,
    ) -> Self {
        let error = kind.sentence(encoder, device.as_deref());
        Self::failed(encoder, available, kind, device, error)
    }
}

/// A device to run a test encode on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestDevice {
    /// Render node for VA-API and Quick Sync; `None` otherwise.
    pub path: Option<String>,
    pub access: NodeAccess,
}

impl TestDevice {
    fn any() -> Self {
        Self {
            path: None,
            access: NodeAccess::Ok,
        }
    }
}

/// What to do for one encoder.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckPlan {
    /// The result is known without running ffmpeg.
    Known(EncoderCheck),
    /// Try a test encode on each device in turn; the first that works wins.
    Test {
        encoder: EncoderInfo,
        devices: Vec<TestDevice>,
    },
}

/// Render nodes of GPUs from `vendors` worth testing, or why there are none.
fn render_node_devices(
    devices: &Devices,
    vendors: &[GpuVendor],
) -> Result<Vec<TestDevice>, (FailureKind, Option<String>)> {
    let gpus: Vec<_> = devices
        .gpus
        .iter()
        .filter(|g| vendors.contains(&g.vendor))
        .collect();
    if gpus.is_empty() {
        return Err((FailureKind::NoDevice, None));
    }
    let nodes: Vec<_> = gpus.iter().filter_map(|g| g.render_node.as_ref()).collect();
    let Some(first) = nodes.first() else {
        return Err((FailureKind::DriverNotLoaded, None));
    };
    let usable: Vec<TestDevice> = nodes
        .iter()
        .filter(|n| n.access != NodeAccess::Missing)
        .map(|n| TestDevice {
            path: Some(n.path.clone()),
            access: n.access.clone(),
        })
        .collect();
    if usable.is_empty() {
        return Err((FailureKind::DeviceMissing, Some(first.path.clone())));
    }
    Ok(usable)
}

/// Devices a hardware API could run on here, or why it can't run at all.
fn candidate_devices(
    api: HwApi,
    devices: &Devices,
    platform: Platform,
) -> Result<Vec<TestDevice>, (FailureKind, Option<String>)> {
    let only_if = |ok: bool| {
        if ok {
            Ok(vec![TestDevice::any()])
        } else {
            Err((FailureKind::NoDevice, None))
        }
    };
    match api {
        HwApi::Software => Ok(vec![TestDevice::any()]),
        HwApi::Nvenc => only_if(devices.nvidia_visible()),
        HwApi::Vaapi => render_node_devices(devices, &[GpuVendor::Intel, GpuVendor::Amd]),
        HwApi::Qsv => render_node_devices(devices, &[GpuVendor::Intel]),
        HwApi::Amf => only_if(devices.has_vendor(GpuVendor::Amd)),
        HwApi::VideoToolbox => only_if(platform.macos),
        HwApi::Rkmpp => only_if(platform.arm && (devices.rockchip_vpu || devices.mpp_present)),
        HwApi::V4l2m2m => only_if(platform.arm),
    }
}

/// Decide, for every encoder in the registry, whether it is missing from the
/// build, verified without a test (software), impossible here (no suitable
/// device), or worth a test encode.
///
/// When `verify` is false, hardware encoders are reported unverified without
/// a test; software encoders never need one.
pub fn plan_checks(
    listed_video: &BTreeSet<String>,
    devices: &Devices,
    platform: Platform,
    verify: bool,
) -> Vec<CheckPlan> {
    VIDEO_ENCODERS
        .iter()
        .map(|encoder| {
            if !listed_video.contains(encoder.name) {
                return CheckPlan::Known(EncoderCheck::known_failure(
                    encoder,
                    false,
                    FailureKind::NotInBuild,
                    None,
                ));
            }
            if encoder.api == HwApi::Software {
                return CheckPlan::Known(EncoderCheck::verified(encoder, None));
            }
            if !verify {
                return CheckPlan::Known(EncoderCheck::known_failure(
                    encoder,
                    true,
                    FailureKind::NotTested,
                    None,
                ));
            }
            match candidate_devices(encoder.api, devices, platform) {
                Ok(devices) => CheckPlan::Test {
                    encoder: *encoder,
                    devices,
                },
                Err((kind, device)) => {
                    CheckPlan::Known(EncoderCheck::known_failure(encoder, true, kind, device))
                }
            }
        })
        .collect()
}

/// Test one encoder on each device until one works.
async fn test_encoder(
    ffmpeg: &Path,
    encoder: EncoderInfo,
    devices: Vec<TestDevice>,
    timeout: Duration,
) -> EncoderCheck {
    let mut first_failure: Option<EncoderCheck> = None;
    for device in &devices {
        let args = test_encode_args(&encoder, device.path.as_deref());
        match run_test_encode(ffmpeg, &args, timeout).await {
            Ok(()) => {
                tracing::debug!(encoder = encoder.name, device = ?device.path, "test encode succeeded");
                return EncoderCheck::verified(&encoder, device.path.clone());
            }
            Err(failure) => {
                let mut kind = classify_failure(&failure.stderr, failure.timed_out);
                // VA-API reports an unopenable node only as "No VA display";
                // the permission check made during the scan is more precise.
                if device.access == NodeAccess::PermissionDenied
                    && matches!(
                        kind,
                        FailureKind::VaapiUnavailable
                            | FailureKind::QsvRuntimeMissing
                            | FailureKind::Other
                    )
                {
                    kind = FailureKind::Permission;
                }
                let error =
                    describe_failure(kind, &encoder, device.path.as_deref(), &failure.stderr);
                tracing::debug!(encoder = encoder.name, device = ?device.path, %error, "test encode failed");
                if first_failure.is_none() {
                    first_failure = Some(EncoderCheck::failed(
                        &encoder,
                        true,
                        kind,
                        device.path.clone(),
                        error,
                    ));
                }
            }
        }
    }
    first_failure
        .unwrap_or_else(|| EncoderCheck::known_failure(&encoder, true, FailureKind::Other, None))
}

/// Carry out the plans: known results are passed through, test encodes run
/// with at most [`TEST_CONCURRENCY`] at a time. Results keep plan order.
pub async fn run_checks(
    ffmpeg: &Path,
    plans: Vec<CheckPlan>,
    timeout: Duration,
) -> Vec<EncoderCheck> {
    let semaphore = Arc::new(Semaphore::new(TEST_CONCURRENCY));
    let mut results: Vec<Option<EncoderCheck>> = vec![None; plans.len()];
    let mut order: Vec<Option<EncoderInfo>> = vec![None; plans.len()];
    let mut tasks = JoinSet::new();

    for (index, plan) in plans.into_iter().enumerate() {
        match plan {
            CheckPlan::Known(check) => results[index] = Some(check),
            CheckPlan::Test { encoder, devices } => {
                order[index] = Some(encoder);
                let semaphore = Arc::clone(&semaphore);
                let ffmpeg = ffmpeg.to_path_buf();
                tasks.spawn(async move {
                    // The semaphore is never closed; if it were, run anyway.
                    let _permit = semaphore.acquire_owned().await.ok();
                    (
                        index,
                        test_encoder(&ffmpeg, encoder, devices, timeout).await,
                    )
                });
            }
        }
    }
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((index, check)) => results[index] = Some(check),
            Err(err) => tracing::warn!(error = %err, "an encoder test stopped unexpectedly"),
        }
    }
    results
        .into_iter()
        .zip(order)
        .filter_map(|(result, encoder)| {
            result.or_else(|| {
                encoder.map(|e| EncoderCheck::known_failure(&e, true, FailureKind::Other, None))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::{DetectedGpu, RenderNode};
    use chrysopoeia_core::{VideoCodec, encoder::find_encoder};

    const ENCODERS_FIXTURE: &str = "\
Encoders:
 V..... = Video
 A..... = Audio
 S..... = Subtitle
 .F.... = Frame-level multithreading
 ..S... = Slice-level multithreading
 ...X.. = Codec is experimental
 ....B. = Supports draw_horiz_band
 .....D = Supports direct rendering method 1
 ------
 V....D a64multi             Multicolor charset for Commodore 64 (codec a64_multi)
 V....D libaom-av1           libaom AV1 (codec av1)
 V..... libsvtav1            SVT-AV1(Scalable Video Technology for AV1) encoder (codec av1)
 V....D av1_nvenc            NVIDIA NVENC av1 encoder (codec av1)
 V..... av1_qsv              AV1 (Intel Quick Sync Video acceleration) (codec av1)
 V....D av1_vaapi            AV1 (VAAPI) (codec av1)
 V....D libx264              libx264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (codec h264)
 V....D h264_nvenc           NVIDIA NVENC H.264 encoder (codec h264)
 V..... h264_qsv             H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (Intel Quick Sync Video acceleration) (codec h264)
 V....D h264_vaapi           H.264/AVC (VAAPI) (codec h264)
 V....D libx265              libx265 H.265 / HEVC (codec hevc)
 V....D hevc_nvenc           NVIDIA NVENC hevc encoder (codec hevc)
 V..... hevc_qsv             HEVC (Intel Quick Sync Video acceleration) (codec hevc)
 V....D hevc_vaapi           H.265/HEVC (VAAPI) (codec hevc)
 V..... hevc_v4l2m2m         V4L2 mem2mem HEVC encoder wrapper (codec hevc)
 V....D libvpx-vp9           libvpx VP9 (codec vp9)
 VF...D ffvhuff              Huffyuv FFmpeg variant
 A....D aac                  AAC (Advanced Audio Coding)
 A....D ac3                  ATSC A/52A (AC-3)
 A....D eac3                 ATSC A/52 E-AC-3
 A....D flac                 FLAC (Free Lossless Audio Codec)
 A....D libmp3lame           libmp3lame MP3 (MPEG audio layer 3) (codec mp3)
 A..X.D opus                 Opus
 A....D libopus              libopus Opus (codec opus)
 A....D libvorbis            libvorbis (codec vorbis)
 S..... srt                  SubRip subtitle
";

    const FILTERS_FIXTURE: &str = "\
Filters:
  T.. = Timeline support
  .S. = Slice threading
  ..C = Command support
  A = Audio input/output
  V = Video input/output
  N = Dynamic number and/or type of input/output
  | = Source or sink filter
 ... abench            A->A       Benchmark part of a filtergraph.
 .S. blackdetect       V->V       Detect video intervals that are (almost) black.
 TS. bwdif             V->V       Deinterlace the input image.
 ... freezedetect      V->V       Detects frozen video input.
 TS. psnr              VV->V      Calculate the PSNR between two video streams.
 TS. ssim              VV->V      Calculate the SSIM between two video streams.
 ... color             |->V       Provide an uniformly colored input.
";

    fn enc(name: &str) -> EncoderInfo {
        *find_encoder(name).unwrap()
    }

    fn intel_gpu(path: &str, access: NodeAccess) -> DetectedGpu {
        DetectedGpu {
            vendor: GpuVendor::Intel,
            name: Some("Intel UHD Graphics 630".into()),
            sysfs_name: None,
            pci_slot: Some("0000:00:02.0".into()),
            device_id: None,
            driver: Some("i915".into()),
            render_node: Some(RenderNode {
                path: path.into(),
                access,
                gid: Some(105),
            }),
        }
    }

    const LINUX_X86: Platform = Platform {
        linux: true,
        macos: false,
        arm: false,
    };

    #[test]
    fn parses_encoder_listing() {
        let listed = parse_encoders(ENCODERS_FIXTURE);
        for name in [
            "libsvtav1",
            "libaom-av1",
            "libx264",
            "libx265",
            "libvpx-vp9",
            "h264_nvenc",
            "hevc_v4l2m2m",
            "ffvhuff",
        ] {
            assert!(listed.video.contains(name), "{name} missing");
        }
        // Legend lines and subtitles are not encoders we track.
        assert!(!listed.video.contains("="));
        assert!(!listed.video.contains("srt"));
        assert!(listed.audio.contains("libopus"));
        assert!(listed.audio.contains("opus"));
        assert_eq!(
            available_audio_encoders(&listed.audio),
            [
                "libopus",
                "aac",
                "flac",
                "ac3",
                "eac3",
                "libmp3lame",
                "libvorbis"
            ]
        );
    }

    #[test]
    fn parses_filter_listing() {
        let filters = parse_filters(FILTERS_FIXTURE);
        assert!(filters.contains("ssim"));
        assert!(filters.contains("color"));
        assert!(!filters.contains("="));
        assert!(!filters.contains("Audio"));
        assert_eq!(available_filters(&filters), VERIFICATION_FILTERS);
        let mut partial = filters.clone();
        partial.remove("psnr");
        assert_eq!(
            available_filters(&partial),
            ["ssim", "blackdetect", "freezedetect", "bwdif"]
        );
    }

    #[test]
    fn test_args_per_api() {
        let common_head = ["-hide_banner", "-nostdin", "-loglevel", "error"];
        let tail = |name: &str| {
            vec![
                "-c:v".to_string(),
                name.into(),
                "-frames:v".into(),
                "25".into(),
                "-f".into(),
                "null".into(),
                "-".into(),
            ]
        };

        let nvenc = test_encode_args(&enc("hevc_nvenc"), None);
        assert_eq!(&nvenc[..4], common_head);
        assert_eq!(
            nvenc[4..].join(" "),
            "-f lavfi -i color=c=black:s=256x256:r=25:d=1 -pix_fmt yuv420p -c:v hevc_nvenc -frames:v 25 -f null -"
        );

        let vaapi = test_encode_args(&enc("h264_vaapi"), Some("/dev/dri/renderD129"));
        assert_eq!(
            vaapi[4..].join(" "),
            "-init_hw_device vaapi=va:/dev/dri/renderD129 -filter_hw_device va \
             -f lavfi -i color=c=black:s=256x256:r=25:d=1 -vf format=nv12,hwupload \
             -c:v h264_vaapi -frames:v 25 -f null -"
        );

        let qsv = test_encode_args(&enc("av1_qsv"), Some("/dev/dri/renderD128"));
        assert_eq!(
            qsv[4..].join(" "),
            "-init_hw_device vaapi=va:/dev/dri/renderD128 -init_hw_device qsv=qs@va -filter_hw_device qs \
             -f lavfi -i color=c=black:s=256x256:r=25:d=1 -vf format=nv12,hwupload=extra_hw_frames=64 \
             -c:v av1_qsv -frames:v 25 -f null -"
        );
        // Without a device, VA-API and QSV use the first render node.
        assert!(
            test_encode_args(&enc("hevc_qsv"), None)
                .contains(&"vaapi=va:/dev/dri/renderD128".to_string())
        );

        for (name, pix) in [
            ("h264_amf", "yuv420p"),
            ("hevc_videotoolbox", "yuv420p"),
            ("hevc_rkmpp", "nv12"),
            ("h264_v4l2m2m", "nv12"),
        ] {
            let args = test_encode_args(&enc(name), None);
            assert_eq!(&args[..4], common_head);
            assert!(!args.contains(&"-init_hw_device".to_string()), "{name}");
            let pos = args.iter().position(|a| a == "-pix_fmt").unwrap();
            assert_eq!(args[pos + 1], pix, "{name}");
            assert_eq!(args[args.len() - 7..], tail(name)[..], "{name}");
        }
    }

    #[test]
    fn classifies_real_failures() {
        let cases = [
            (
                "[h264_nvenc @ 0x562ae7bc1ec0] Cannot load libcuda.so.1\n\
                 [vost#0:0/h264_nvenc @ 0x562ae7bc1b00] Error while opening encoder - maybe incorrect parameters such as bit_rate, rate, width or height.\n\
                 Error while filtering: Operation not permitted\n",
                FailureKind::NvidiaDriverMissing,
            ),
            (
                "[hevc_nvenc @ 0x1] cuInit(0) failed -> CUDA_ERROR_NO_DEVICE: no CUDA-capable device is detected\n",
                FailureKind::NvidiaDriverMissing,
            ),
            (
                "[hevc_nvenc @ 0x1] OpenEncodeSessionEx failed: incompatible client key (21): (no details)\n",
                FailureKind::NvencSessionLimit,
            ),
            (
                "[h264_nvenc @ 0x1] OpenEncodeSessionEx failed: out of memory (10): (no details)\n",
                FailureKind::NvencSessionLimit,
            ),
            (
                "[h264_nvenc @ 0x1] Driver does not support the required nvenc API version. Required: 12.1 Found: 11.1\n",
                FailureKind::NvidiaDriverTooOld,
            ),
            (
                "[av1_nvenc @ 0x1] Codec not supported\n[av1_nvenc @ 0x1] No capable devices found\n",
                FailureKind::CodecUnsupported,
            ),
            (
                "[AVHWDeviceContext @ 0x557c49edaf00] No VA display found for device /dev/dri/renderD128.\n\
                 Device creation failed: -22.\n\
                 Failed to set value 'vaapi=va:/dev/dri/renderD128' for option 'init_hw_device': Invalid argument\n",
                FailureKind::VaapiUnavailable,
            ),
            (
                "[AVHWDeviceContext @ 0x1] Failed to initialise VAAPI connection: -1 (unknown libva error).\n",
                FailureKind::VaapiUnavailable,
            ),
            (
                "[AVHWDeviceContext @ 0x1] Error creating a MFX session: -9.\nDevice creation failed: -1313558101.\nFailed to set value 'qsv=qs@va' for option 'init_hw_device': Unknown error occurred\n",
                FailureKind::QsvRuntimeMissing,
            ),
            (
                "[av1_vaapi @ 0x1] No usable encoding profile found.\n",
                FailureKind::CodecUnsupported,
            ),
            (
                "[hevc_qsv @ 0x1] Current profile is not supported\n",
                FailureKind::CodecUnsupported,
            ),
            (
                "[AVHWDeviceContext @ 0x1] Failed to open /dev/dri/renderD128: Permission denied\n",
                FailureKind::Permission,
            ),
            (
                "DLL libamfrt64.so.1 failed to open\n",
                FailureKind::AmfRuntimeMissing,
            ),
            (
                "[h264_v4l2m2m @ 0x1] Could not find a valid device\n[h264_v4l2m2m @ 0x1] can't configure encoder\n",
                FailureKind::NoEncoderDevice,
            ),
            (
                "[hevc_rkmpp @ 0x1] Failed to init MPP context (code = -1)\n",
                FailureKind::RkmppUnavailable,
            ),
            ("Something unexpected happened\n", FailureKind::Other),
        ];
        for (stderr, expected) in cases {
            assert_eq!(classify_failure(stderr, false), expected, "{stderr}");
        }
        assert_eq!(classify_failure("", true), FailureKind::TimedOut);
    }

    #[test]
    fn failure_messages_are_plain() {
        let msg = describe_failure(
            FailureKind::NvidiaDriverMissing,
            &enc("h264_nvenc"),
            None,
            "[h264_nvenc @ 0x562ae7bc1ec0] Cannot load libcuda.so.1\nline two\nline three\nline four\n",
        );
        assert_eq!(
            msg,
            "The NVIDIA driver isn't available inside the container. Details: line two / line three / line four"
        );
        assert_eq!(
            FailureKind::CodecUnsupported.sentence(&enc("av1_vaapi"), None),
            "This GPU can't encode AV1."
        );
        assert_eq!(
            FailureKind::CodecUnsupported.sentence(&enc("hevc_qsv"), None),
            "This GPU can't encode HEVC."
        );
        assert_eq!(stderr_tail("[x @ 0xabc] only line\n", 3), "only line");
        assert_eq!(stderr_tail(&"x".repeat(300), 1).chars().count(), 203);
        assert_eq!(
            FailureKind::TimedOut.sentence(&enc("h264_qsv"), None),
            "The test encode timed out."
        );
    }

    #[test]
    fn plans_only_plausible_tests() {
        let listed = parse_encoders(ENCODERS_FIXTURE).video;
        let find = |plans: &[CheckPlan], name: &str| -> CheckPlan {
            let idx = VIDEO_ENCODERS.iter().position(|e| e.name == name).unwrap();
            plans[idx].clone()
        };

        // No GPU at all.
        let none = Devices::default();
        let plans = plan_checks(&listed, &none, LINUX_X86, true);
        assert_eq!(plans.len(), VIDEO_ENCODERS.len());
        assert!(plans.iter().all(|p| matches!(p, CheckPlan::Known(_))));
        let CheckPlan::Known(nvenc) = find(&plans, "h264_nvenc") else {
            panic!()
        };
        assert!(nvenc.status.available && !nvenc.status.verified);
        assert_eq!(
            nvenc.status.error.as_deref(),
            Some("No NVIDIA GPU is visible to Chrysopoeia.")
        );
        let CheckPlan::Known(x264) = find(&plans, "libx264") else {
            panic!()
        };
        assert!(x264.status.verified && x264.failure.is_none());
        let CheckPlan::Known(amf) = find(&plans, "h264_amf") else {
            panic!()
        };
        assert!(!amf.status.available);
        assert_eq!(amf.failure, Some(FailureKind::NotInBuild));
        let CheckPlan::Known(v4l2) = find(&plans, "hevc_v4l2m2m") else {
            panic!()
        };
        assert_eq!(v4l2.failure, Some(FailureKind::NoDevice));

        // Intel iGPU + NVIDIA: NVENC, QSV and VA-API get tested.
        let devices = Devices {
            nvidia_device_nodes: 1,
            gpus: vec![intel_gpu("/dev/dri/renderD128", NodeAccess::Ok)],
            ..Devices::default()
        };
        let plans = plan_checks(&listed, &devices, LINUX_X86, true);
        for name in ["h264_nvenc", "hevc_qsv", "av1_vaapi"] {
            let CheckPlan::Test { devices, .. } = find(&plans, name) else {
                panic!("{name} not tested")
            };
            assert_eq!(devices.len(), 1);
        }
        let CheckPlan::Test {
            devices: qsv_devices,
            ..
        } = find(&plans, "hevc_qsv")
        else {
            panic!()
        };
        assert_eq!(qsv_devices[0].path.as_deref(), Some("/dev/dri/renderD128"));
        // ARM-only encoders are not tested on x86.
        assert!(matches!(find(&plans, "hevc_v4l2m2m"), CheckPlan::Known(_)));
        // ...but are on ARM.
        let arm = Platform {
            arm: true,
            ..LINUX_X86
        };
        assert!(matches!(
            find(&plan_checks(&listed, &devices, arm, true), "hevc_v4l2m2m"),
            CheckPlan::Test { .. }
        ));

        // Rockchip MPP is only tried on boards that have it.
        let mut with_rkmpp = listed.clone();
        with_rkmpp.insert("hevc_rkmpp".to_string());
        let plain_arm = plan_checks(&with_rkmpp, &Devices::default(), arm, true);
        let CheckPlan::Known(rk) = find(&plain_arm, "hevc_rkmpp") else {
            panic!()
        };
        assert_eq!(rk.failure, Some(FailureKind::NoDevice));
        let rockchip = Devices {
            rockchip_vpu: true,
            mpp_present: true,
            ..Devices::default()
        };
        assert!(matches!(
            find(
                &plan_checks(&with_rkmpp, &rockchip, arm, true),
                "hevc_rkmpp"
            ),
            CheckPlan::Test { .. }
        ));

        // Checks turned off: hardware unverified, software still verified.
        let plans = plan_checks(&listed, &devices, LINUX_X86, false);
        let CheckPlan::Known(off) = find(&plans, "h264_nvenc") else {
            panic!()
        };
        assert_eq!(off.failure, Some(FailureKind::NotTested));
        let CheckPlan::Known(sw) = find(&plans, "libsvtav1") else {
            panic!()
        };
        assert!(sw.status.verified);

        // A render node that wasn't passed into the container isn't tested.
        let missing = Devices {
            gpus: vec![intel_gpu("/dev/dri/renderD128", NodeAccess::Missing)],
            ..Devices::default()
        };
        let CheckPlan::Known(check) = find(
            &plan_checks(&listed, &missing, LINUX_X86, true),
            "h264_vaapi",
        ) else {
            panic!()
        };
        assert_eq!(check.failure, Some(FailureKind::DeviceMissing));
        assert_eq!(check.status.device.as_deref(), Some("/dev/dri/renderD128"));
        assert_eq!(
            check.status.error.as_deref(),
            Some("/dev/dri/renderD128 is missing, so the GPU isn't available to Chrysopoeia.")
        );

        // An Intel GPU without a driver: nothing to test.
        let mut no_driver = intel_gpu("/dev/dri/renderD128", NodeAccess::Ok);
        no_driver.render_node = None;
        let devices = Devices {
            gpus: vec![no_driver],
            ..Devices::default()
        };
        let CheckPlan::Known(check) =
            find(&plan_checks(&listed, &devices, LINUX_X86, true), "h264_qsv")
        else {
            panic!()
        };
        assert_eq!(check.failure, Some(FailureKind::DriverNotLoaded));
    }

    fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[tokio::test]
    async fn real_test_encodes_are_classified() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not found; skipping");
            return;
        }
        let ffmpeg = Path::new("ffmpeg");
        let probe = probe_ffmpeg(ffmpeg, Path::new("ffprobe")).await;
        assert!(probe.info.found);

        // A software encode through the same runner succeeds.
        let x264 = enc("libx264");
        run_test_encode(ffmpeg, &test_encode_args(&x264, None), TEST_TIMEOUT)
            .await
            .expect("libx264 test encode should work");

        // Hardware tests on a machine without the hardware fail with a
        // useful classification, quickly.
        let mut plans = Vec::new();
        let mut expected = Vec::new();
        if probe.encoders.video.contains("h264_nvenc") && !Path::new("/dev/nvidia0").exists() {
            plans.push(CheckPlan::Test {
                encoder: enc("h264_nvenc"),
                devices: vec![TestDevice::any()],
            });
            expected.push(FailureKind::NvidiaDriverMissing);
        }
        if probe.encoders.video.contains("h264_vaapi") {
            plans.push(CheckPlan::Test {
                encoder: enc("h264_vaapi"),
                devices: vec![TestDevice {
                    path: Some("/nonexistent/renderD128".into()),
                    access: NodeAccess::Ok,
                }],
            });
            expected.push(FailureKind::VaapiUnavailable);
        }
        let started = std::time::Instant::now();
        let checks = run_checks(ffmpeg, plans, TEST_TIMEOUT).await;
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(checks.len(), expected.len());
        for (check, kind) in checks.iter().zip(expected) {
            assert!(!check.status.verified, "{:?}", check.status);
            assert_eq!(check.failure, Some(kind), "{:?}", check.status);
            let error = check.status.error.as_deref().unwrap();
            assert!(error.contains("Details:"), "{error}");
        }
    }

    #[tokio::test]
    async fn permission_denied_node_is_reported_as_permission() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not found; skipping");
            return;
        }
        let probe = probe_ffmpeg(Path::new("ffmpeg"), Path::new("ffprobe")).await;
        if !probe.encoders.video.contains("hevc_vaapi") {
            eprintln!("ffmpeg lacks VA-API; skipping");
            return;
        }
        let check = test_encoder(
            Path::new("ffmpeg"),
            enc("hevc_vaapi"),
            vec![TestDevice {
                path: Some("/nonexistent/renderD128".into()),
                access: NodeAccess::PermissionDenied,
            }],
            TEST_TIMEOUT,
        )
        .await;
        assert_eq!(check.failure, Some(FailureKind::Permission));
        assert!(
            check
                .status
                .error
                .as_deref()
                .unwrap()
                .starts_with("Chrysopoeia doesn't have permission to use /nonexistent/renderD128.")
        );
    }

    #[tokio::test]
    async fn missing_ffmpeg_is_reported() {
        let probe = probe_ffmpeg(
            Path::new("/nonexistent/ffmpeg"),
            Path::new("/nonexistent/ffprobe"),
        )
        .await;
        assert!(!probe.info.found);
        assert!(!probe.info.ffprobe_found);
        assert_eq!(probe.info.version, None);
        assert!(probe.encoders.video.is_empty());
        assert_eq!(probe.info.ffmpeg_path, "/nonexistent/ffmpeg");
    }

    #[test]
    fn video_codec_label_used_for_h264() {
        assert_eq!(
            FailureKind::CodecUnsupported.sentence(&enc("h264_vaapi"), None),
            format!("This GPU can't encode {}.", VideoCodec::H264.label())
        );
    }
}
