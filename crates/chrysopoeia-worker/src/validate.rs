//! Output verification: structure, full decode, and visual comparison
//! against the source.
//!
//! Checks by level (each has a stable id and a plain-language label):
//!
//! | id | label | level |
//! |---|---|---|
//! | `probe` | Opens correctly | quick+ |
//! | `streams` | All tracks present | quick+ |
//! | `duration` | Same length as the original | quick+ |
//! | `size` | Smaller than the original (informational) | quick+ |
//! | `decode` | Plays start to finish | standard+ |
//! | `visual` | Looks like the original | standard+ |
//! | `black_frames` | No extra black frames | thorough |
//! | `frozen_frames` | No frozen frames | thorough |
//!
//! Checks run in that order and stop at the first failure (the remaining
//! checks are reported as skipped), so a broken hardware encode falls back
//! quickly.
//!
//! The visual comparison decodes the same short segment from the source and
//! the output (seeking relative to each file's video start), scales the
//! source to the output size (deinterlacing it with `bwdif` when the output
//! was deinterlaced), puts both on the same frame grid and measures SSIM and
//! PSNR per frame. Corruption (green or grey frames, heavy blocking, wrong
//! content) shows up as very low SSIM.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use chrysopoeia_core::{
    CheckStatus, Container, ProbeInfo, StreamKind, TranscodeProfile, ValidationCheck,
    ValidationLevel, ValidationReport, VideoCodec,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::ffmpeg::{
    FfmpegCommand, FfmpegExit, ProgressBlock, compute_progress, run_ffmpeg, split_log_line,
};
use crate::plan::StreamSummary;

/// A frame below this SSIM means corruption.
pub const FRAME_SSIM_FAIL: f64 = 0.60;
/// A segment whose mean SSIM is below this does not match the source.
pub const SEGMENT_SSIM_FAIL: f64 = 0.85;
/// An overall mean SSIM below this is reported as a warning.
pub const OVERALL_SSIM_WARN: f64 = 0.93;
/// Length of each compared segment, in seconds.
pub const SEGMENT_SECS: f64 = 2.0;
/// Segments compared at the standard level.
pub const STANDARD_SEGMENTS: usize = 4;
/// Segments compared at the thorough level.
pub const THOROUGH_SEGMENTS: usize = 10;
/// The output may add at most this much black or frozen video (seconds).
pub const MAX_ADDED_SECS: f64 = 2.0;

/// Fraction of the file skipped at each end for the visual comparison
/// (intros, credits and fades are poor comparison material).
const EDGE_FRACTION: f64 = 0.05;
/// The source is seeked this far before each segment and cut precisely with
/// `trim`: seeking in MPEG-TS and similar files can land after the requested
/// time, which would misalign the comparison.
const SEEK_PREROLL_SECS: f64 = 5.0;
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const DECODE_STALL: Duration = Duration::from_secs(600);
const SEGMENT_STALL: Duration = Duration::from_secs(180);
const BLACKDETECT: &str = "blackdetect=d=0.5:pix_th=0.10";
const FREEZEDETECT: &str = "freezedetect=n=-60dB:d=0.5";
/// PSNR reported for identical pictures (the true value is infinite).
const PSNR_CAP: f64 = 100.0;

/// Inputs for [`validate_output`].
#[derive(Debug, Clone, Copy)]
pub struct ValidateRequest<'a> {
    /// The ffmpeg binary.
    pub ffmpeg: &'a Path,
    /// The ffprobe binary.
    pub ffprobe: &'a Path,
    /// The original file.
    pub source: &'a Path,
    /// What the scanner found in the original.
    pub source_probe: &'a ProbeInfo,
    /// The encoded file to check.
    pub output: &'a Path,
    /// The profile it was encoded for (target video codec, container).
    pub profile: &'a TranscodeProfile,
    /// Which checks to run.
    pub level: ValidationLevel,
    /// Tracks the output should contain (from the plan).
    pub expected: StreamSummary,
}

/// Verify an encoded file. `on_progress` receives 0..=100.
///
/// Never panics. If `cancel` fires, the remaining checks are reported as
/// skipped and `passed` is false.
pub async fn validate_output(
    req: &ValidateRequest<'_>,
    cancel: &CancellationToken,
    on_progress: &(dyn Fn(f32) + Send + Sync),
) -> ValidationReport {
    validate_output_at(req, false, cancel, on_progress).await
}

/// [`validate_output`], optionally running the decoders under `nice` (the
/// job's low-priority setting also covers its verification).
pub(crate) async fn validate_output_at(
    req: &ValidateRequest<'_>,
    low_priority: bool,
    cancel: &CancellationToken,
    on_progress: &(dyn Fn(f32) + Send + Sync),
) -> ValidationReport {
    let started = Instant::now();
    let mut state = State::default();
    if req.level != ValidationLevel::Off {
        let ctx = Ctx {
            req,
            cancel,
            on_progress,
            weights: Weights::for_level(req.level),
            low_priority,
        };
        state.stop = ctx.run(&mut state).await.err();
    }
    on_progress(100.0);
    state.into_report(req.level, started.elapsed().as_secs_f64())
}

// ---------------------------------------------------------------------------
// Check bookkeeping

/// Why checking stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Failed,
    Cancelled,
}

#[derive(Debug, Default)]
struct State {
    checks: Vec<ValidationCheck>,
    ssim_min: Option<f64>,
    ssim_avg: Option<f64>,
    psnr_avg: Option<f64>,
    stop: Option<Stop>,
}

impl State {
    fn push(&mut self, id: CheckId, status: CheckStatus, detail: String, value: Option<f64>) {
        self.checks.push(ValidationCheck {
            id: id.id().to_string(),
            label: id.label().to_string(),
            status,
            detail,
            value,
        });
    }

    /// Record a result; a failure stops the run.
    fn record(&mut self, id: CheckId, result: CheckResult) -> Result<(), Stop> {
        let failed = result.status == CheckStatus::Fail;
        self.push(id, result.status, result.detail, result.value);
        if failed { Err(Stop::Failed) } else { Ok(()) }
    }

    fn into_report(mut self, level: ValidationLevel, elapsed_secs: f64) -> ValidationReport {
        let skipped_detail = match self.stop {
            Some(Stop::Cancelled) => "Verification was cancelled",
            _ => "Skipped because an earlier check failed",
        };
        for id in CheckId::for_level(level) {
            if !self.checks.iter().any(|c| c.id == id.id()) {
                self.push(*id, CheckStatus::Skipped, skipped_detail.to_string(), None);
            }
        }
        let order = |c: &ValidationCheck| {
            CheckId::ALL
                .iter()
                .position(|id| id.id() == c.id)
                .unwrap_or(usize::MAX)
        };
        self.checks.sort_by_key(order);
        let passed = self.stop != Some(Stop::Cancelled)
            && !self.checks.iter().any(|c| c.status == CheckStatus::Fail);
        ValidationReport {
            passed,
            level,
            checks: self.checks,
            ssim_min: self.ssim_min,
            ssim_avg: self.ssim_avg,
            psnr_avg: self.psnr_avg,
            elapsed_secs,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckId {
    Probe,
    Streams,
    Duration,
    Size,
    Decode,
    Visual,
    BlackFrames,
    FrozenFrames,
}

impl CheckId {
    const ALL: [CheckId; 8] = [
        Self::Probe,
        Self::Streams,
        Self::Duration,
        Self::Size,
        Self::Decode,
        Self::Visual,
        Self::BlackFrames,
        Self::FrozenFrames,
    ];

    fn for_level(level: ValidationLevel) -> &'static [CheckId] {
        match level {
            ValidationLevel::Off => &[],
            ValidationLevel::Quick => &Self::ALL[..4],
            ValidationLevel::Standard => &Self::ALL[..6],
            ValidationLevel::Thorough => &Self::ALL,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Probe => "probe",
            Self::Streams => "streams",
            Self::Duration => "duration",
            Self::Size => "size",
            Self::Decode => "decode",
            Self::Visual => "visual",
            Self::BlackFrames => "black_frames",
            Self::FrozenFrames => "frozen_frames",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Probe => "Opens correctly",
            Self::Streams => "All tracks present",
            Self::Duration => "Same length as the original",
            Self::Size => "Smaller than the original",
            Self::Decode => "Plays start to finish",
            Self::Visual => "Looks like the original",
            Self::BlackFrames => "No extra black frames",
            Self::FrozenFrames => "No frozen frames",
        }
    }
}

/// Outcome of one check before it is recorded.
#[derive(Debug, Clone, PartialEq)]
struct CheckResult {
    status: CheckStatus,
    detail: String,
    value: Option<f64>,
}

impl CheckResult {
    fn pass(detail: impl Into<String>, value: Option<f64>) -> Self {
        Self {
            status: CheckStatus::Pass,
            detail: detail.into(),
            value,
        }
    }

    fn warn(detail: impl Into<String>, value: Option<f64>) -> Self {
        Self {
            status: CheckStatus::Warn,
            detail: detail.into(),
            value,
        }
    }

    fn fail(detail: impl Into<String>, value: Option<f64>) -> Self {
        Self {
            status: CheckStatus::Fail,
            detail: detail.into(),
            value,
        }
    }

    fn skipped(detail: impl Into<String>) -> Self {
        Self {
            status: CheckStatus::Skipped,
            detail: detail.into(),
            value: None,
        }
    }
}

/// Share of the overall progress bar given to each phase.
#[derive(Debug, Clone, Copy)]
struct Weights {
    probe: (f32, f32),
    decode: (f32, f32),
    visual: (f32, f32),
    source_scan: (f32, f32),
}

impl Weights {
    fn for_level(level: ValidationLevel) -> Self {
        match level {
            ValidationLevel::Off | ValidationLevel::Quick => Self {
                probe: (0.0, 100.0),
                decode: (100.0, 100.0),
                visual: (100.0, 100.0),
                source_scan: (100.0, 100.0),
            },
            ValidationLevel::Standard => Self {
                probe: (0.0, 5.0),
                decode: (5.0, 75.0),
                visual: (75.0, 100.0),
                source_scan: (100.0, 100.0),
            },
            ValidationLevel::Thorough => Self {
                probe: (0.0, 3.0),
                decode: (3.0, 40.0),
                visual: (40.0, 55.0),
                source_scan: (55.0, 100.0),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// The checks

struct Ctx<'r, 'a> {
    req: &'r ValidateRequest<'a>,
    cancel: &'r CancellationToken,
    on_progress: &'r (dyn Fn(f32) + Send + Sync),
    weights: Weights,
    /// Run decoders under `nice`.
    low_priority: bool,
}

impl Ctx<'_, '_> {
    fn progress(&self, (start, end): (f32, f32), fraction: f32) {
        let pct = start + (end - start) * fraction.clamp(0.0, 1.0);
        (self.on_progress)(pct.clamp(0.0, 100.0));
    }

    fn check_cancel(&self) -> Result<(), Stop> {
        if self.cancel.is_cancelled() {
            Err(Stop::Cancelled)
        } else {
            Ok(())
        }
    }

    async fn run(&self, state: &mut State) -> Result<(), Stop> {
        let req = self.req;
        self.progress(self.weights.probe, 0.0);

        // Opens correctly.
        let output = match probe_media(req.ffprobe, req.output, self.cancel).await {
            Ok(p) => p,
            Err(ProbeError::Cancelled) => return Err(Stop::Cancelled),
            Err(ProbeError::Failed(msg)) => {
                return state.record(
                    CheckId::Probe,
                    CheckResult::fail(format!("The new file could not be opened: {msg}"), None),
                );
            }
        };
        state.record(CheckId::Probe, probe_check(&output, req.profile))?;

        // All tracks present.
        state.record(CheckId::Streams, streams_check(&output, req.expected))?;

        // The source's own probe gives per-stream durations and the video
        // start time for seeking. It is optional: the checks degrade without it.
        let source = match probe_media(req.ffprobe, req.source, self.cancel).await {
            Ok(p) => Some(p),
            Err(ProbeError::Cancelled) => return Err(Stop::Cancelled),
            Err(ProbeError::Failed(msg)) => {
                tracing::debug!("could not probe the source for verification: {msg}");
                None
            }
        };
        let (source_duration, output_duration) =
            comparable_durations(req.source_probe, source.as_ref(), &output);

        // Same length as the original.
        state.record(
            CheckId::Duration,
            duration_check(source_duration, output_duration),
        )?;

        // Smaller than the original (informational).
        let source_size = match tokio::fs::metadata(req.source).await {
            Ok(m) => m.len(),
            Err(_) => req.source_probe.size_bytes,
        };
        let output_size = tokio::fs::metadata(req.output)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        state.record(CheckId::Size, size_check(source_size, output_size))?;
        self.progress(self.weights.probe, 1.0);

        if req.level == ValidationLevel::Quick {
            return Ok(());
        }
        self.check_cancel()?;

        // Plays start to finish (and, when thorough, black/frozen totals of
        // the output from the same decode pass). The decoded length is
        // compared with what the output claims for its picture and sound, so
        // a file cut short on disk fails even though its header looks fine.
        let thorough = req.level == ValidationLevel::Thorough;
        let expected_len = output
            .av_duration()
            .or(output.best_duration())
            .or(source_duration);
        let decode = self.decode_output(&output, expected_len, thorough).await?;
        state.record(CheckId::Decode, decode_check(&decode, expected_len))?;

        // Looks like the original. Segments are placed within the source's
        // video, which may be shorter than its sound or subtitles.
        self.check_cancel()?;
        let segments = if thorough {
            THOROUGH_SEGMENTS
        } else {
            STANDARD_SEGMENTS
        };
        let video_len = source
            .as_ref()
            .and_then(|s| s.primary_video())
            .and_then(|v| v.duration)
            .filter(|d| *d > 0.0)
            .or(source_duration);
        let visual = self
            .visual(&output, source.as_ref(), video_len, segments)
            .await?;
        state.ssim_min = visual.ssim_min;
        state.ssim_avg = visual.ssim_avg;
        state.psnr_avg = visual.psnr_avg;
        state.record(CheckId::Visual, visual.result)?;

        if !thorough {
            return Ok(());
        }

        // No extra black / frozen frames.
        self.check_cancel()?;
        let source_totals = self.scan_source(source.as_ref(), video_len).await?;
        let (black, frozen) = match (&decode.detect, source_totals) {
            (Some(out), Some(src)) => (
                added_time_check(src.black_secs, out.black_secs, "black"),
                added_time_check(src.frozen_secs, out.frozen_secs, "frozen"),
            ),
            _ => (
                CheckResult::skipped("The black-frame scan could not run"),
                CheckResult::skipped("The frozen-frame scan could not run"),
            ),
        };
        state.record(CheckId::BlackFrames, black)?;
        state.record(CheckId::FrozenFrames, frozen)?;
        self.progress(self.weights.source_scan, 1.0);
        Ok(())
    }

    /// Full decode of the output. With `detect`, also totals black and
    /// frozen video in the same pass.
    async fn decode_output(
        &self,
        output: &MediaProbe,
        expected_len: Option<f64>,
        detect: bool,
    ) -> Result<DecodeResult, Stop> {
        let req = self.req;
        let video_map = output
            .primary_video()
            .map_or_else(|| "0:v:0".to_string(), |v| format!("0:{}", v.index));
        let mut args: Vec<String> = [
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            if detect { "level+info" } else { "level+error" },
            "-i",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        args.push(req.output.to_string_lossy().into_owned());
        args.extend(["-map".to_string(), video_map, "-map".into(), "0:a?".into()]);
        if detect {
            args.extend([
                "-filter:v:0".to_string(),
                format!("{BLACKDETECT},{FREEZEDETECT}"),
            ]);
        }
        args.extend(
            ["-progress", "pipe:1", "-nostats", "-f", "null", "-"]
                .iter()
                .map(|s| s.to_string()),
        );

        let mut errors = ErrorLines::default();
        let mut totals = DetectTotals::default();
        let mut decoded_secs: Option<f64> = None;
        let started = Instant::now();
        let weights = self.weights.decode;
        let duration = output.best_duration().or(expected_len);
        let cmd = FfmpegCommand {
            program: req.ffmpeg,
            args: &args,
            low_priority: self.low_priority,
            stall_timeout: DECODE_STALL,
        };
        let exit = run_ffmpeg(
            &cmd,
            self.cancel,
            &mut |block: &ProgressBlock| {
                if let Some(t) = block.out_time_secs {
                    decoded_secs = Some(decoded_secs.map_or(t, |d: f64| d.max(t)));
                }
                let p = compute_progress(block, duration, started.elapsed().as_secs_f64());
                self.progress(weights, p.percent / 100.0);
            },
            &mut |line: &str| {
                errors.push(line, !detect);
                if detect {
                    totals.push_line(line);
                }
            },
        )
        .await;
        if exit == FfmpegExit::Cancelled {
            return Err(Stop::Cancelled);
        }
        let video = output.primary_video();
        let stream_end = video.and_then(|v| v.start_time).unwrap_or(0.0)
            + video
                .and_then(|v| v.duration)
                .or(output.best_duration())
                .unwrap_or(0.0);
        Ok(DecodeResult {
            exit,
            first_error: errors.first,
            error_count: errors.count,
            decoded_secs,
            detect: detect.then(|| totals.finish(stream_end)),
        })
    }

    /// Compare pictures at `count` points spread over the file.
    async fn visual(
        &self,
        output: &MediaProbe,
        source: Option<&MediaProbe>,
        source_duration: Option<f64>,
        count: usize,
    ) -> Result<VisualOutcome, Stop> {
        let req = self.req;
        let Some(duration) = source_duration.filter(|d| *d >= 1.0) else {
            return Ok(VisualOutcome::only(CheckResult::skipped(
                "The file is too short to compare pictures",
            )));
        };
        let Some(align) = Alignment::new(req.source_probe, source, output) else {
            return Ok(VisualOutcome::only(CheckResult::fail(
                "The video track could not be found for comparison",
                None,
            )));
        };
        let segments = segment_windows(duration, count);
        if segments.is_empty() {
            return Ok(VisualOutcome::only(CheckResult::skipped(
                "The file is too short to compare pictures",
            )));
        }

        let dir = match make_stats_dir().await {
            Ok(dir) => dir,
            Err(e) => {
                return Ok(VisualOutcome::only(CheckResult::skipped(format!(
                    "No scratch space to compare pictures: {e}"
                ))));
            }
        };
        let mut measured = Vec::with_capacity(segments.len());
        let mut failure = None;
        for (i, &(start, len)) in segments.iter().enumerate() {
            self.progress(self.weights.visual, i as f32 / segments.len() as f32);
            match self
                .compare_segment(&align, dir.path(), i, start, len)
                .await
            {
                Ok(seg) => measured.push(seg),
                Err(SegmentError::Cancelled) => {
                    discard_dir(dir).await;
                    return Err(Stop::Cancelled);
                }
                Err(SegmentError::Failed(msg)) => {
                    failure = Some(CheckResult::fail(msg, None));
                    break;
                }
            }
        }
        discard_dir(dir).await;
        self.progress(self.weights.visual, 1.0);

        let mut outcome = summarize_segments(&measured);
        if let Some(fail) = failure {
            outcome.result = fail;
        }
        Ok(outcome)
    }

    /// Compare one segment and parse its SSIM/PSNR statistics. If seeking
    /// in the source overshot the segment (no frames compared), the segment
    /// is compared once more reading the source from its start.
    async fn compare_segment(
        &self,
        align: &Alignment,
        dir: &Path,
        index: usize,
        start: f64,
        len: f64,
    ) -> Result<SegmentStats, SegmentError> {
        let at = align.seek_point(start);
        let (ssim_path, psnr_path) = stats_paths(dir, index);
        for preroll in [Some(SEEK_PREROLL_SECS), None] {
            let window = SegmentWindow {
                at,
                len,
                source_preroll: preroll,
            };
            self.run_segment(align, &window, &ssim_path, &psnr_path, start)
                .await?;
            let ssim = tokio::fs::read_to_string(&ssim_path)
                .await
                .map(|t| parse_ssim_stats(&t))
                .unwrap_or_default();
            let mse = tokio::fs::read_to_string(&psnr_path)
                .await
                .map(|t| parse_psnr_stats(&t))
                .unwrap_or_default();
            if !ssim.is_empty() {
                return Ok(SegmentStats { start, ssim, mse });
            }
            tracing::debug!(
                segment = index,
                "no frames compared; retrying without seeking"
            );
        }
        Err(SegmentError::Failed(format!(
            "No picture could be read from the new file at {}",
            format_time(start)
        )))
    }

    async fn run_segment(
        &self,
        align: &Alignment,
        window: &SegmentWindow,
        ssim_path: &Path,
        psnr_path: &Path,
        start: f64,
    ) -> Result<(), SegmentError> {
        let req = self.req;
        let args = segment_args(req, align, window, ssim_path, psnr_path);
        let cmd = FfmpegCommand {
            program: req.ffmpeg,
            args: &args,
            low_priority: self.low_priority,
            stall_timeout: SEGMENT_STALL,
        };
        match run_ffmpeg(&cmd, self.cancel, &mut |_| {}, &mut |_| {}).await {
            FfmpegExit::Success { .. } => Ok(()),
            FfmpegExit::Cancelled => Err(SegmentError::Cancelled),
            other => {
                let why = other
                    .describe_failure("ffmpeg")
                    .unwrap_or_else(|| "unknown error".into());
                Err(SegmentError::Failed(format!(
                    "The pictures at {} could not be compared ({why})",
                    format_time(start)
                )))
            }
        }
    }

    /// Black/frozen totals of the source (thorough level).
    async fn scan_source(
        &self,
        source: Option<&MediaProbe>,
        source_duration: Option<f64>,
    ) -> Result<Option<DetectTotals>, Stop> {
        let req = self.req;
        let video = source
            .and_then(MediaProbe::primary_video)
            .map(|v| (v.index, v.start_time))
            .or_else(|| req.source_probe.primary_video().map(|v| (v.index, None)));
        let Some((index, start)) = video else {
            return Ok(None);
        };
        let mut args: Vec<String> = ["-nostdin", "-hide_banner", "-loglevel", "level+info", "-i"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        args.push(req.source.to_string_lossy().into_owned());
        args.extend([
            "-map".to_string(),
            format!("0:{index}"),
            "-vf".to_string(),
            format!("{BLACKDETECT},{FREEZEDETECT}"),
        ]);
        args.extend(
            ["-progress", "pipe:1", "-nostats", "-f", "null", "-"]
                .iter()
                .map(|s| s.to_string()),
        );
        let mut totals = DetectTotals::default();
        let started = Instant::now();
        let weights = self.weights.source_scan;
        let cmd = FfmpegCommand {
            program: req.ffmpeg,
            args: &args,
            low_priority: self.low_priority,
            stall_timeout: DECODE_STALL,
        };
        let exit = run_ffmpeg(
            &cmd,
            self.cancel,
            &mut |block: &ProgressBlock| {
                let p = compute_progress(block, source_duration, started.elapsed().as_secs_f64());
                self.progress(weights, p.percent / 100.0);
            },
            &mut |line: &str| totals.push_line(line),
        )
        .await;
        match exit {
            FfmpegExit::Cancelled => Err(Stop::Cancelled),
            FfmpegExit::Success { .. } => {
                let end = start.unwrap_or(0.0) + source_duration.unwrap_or(0.0);
                Ok(Some(totals.finish(end)))
            }
            other => {
                tracing::debug!("black/frozen scan of the source failed: {other:?}");
                Ok(None)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Pure check logic

fn probe_check(output: &MediaProbe, profile: &TranscodeProfile) -> CheckResult {
    let Some(video) = output.primary_video() else {
        return CheckResult::fail("The new file has no video track", None);
    };
    let target = profile.video_codec;
    if VideoCodec::from_probe_name(&video.codec) != Some(target) {
        let found = VideoCodec::from_probe_name(&video.codec)
            .map_or_else(|| video.codec.clone(), |c| c.label().to_string());
        return CheckResult::fail(
            format!("The video is {found} instead of {}", target.label()),
            None,
        );
    }
    let article = match profile.container {
        Container::Mkv | Container::Mp4 => "an",
        Container::Webm => "a",
    };
    CheckResult::pass(
        format!(
            "Opens as {article} {} file with {} video",
            profile.container.label(),
            target.label()
        ),
        None,
    )
}

fn streams_check(output: &MediaProbe, expected: StreamSummary) -> CheckResult {
    let found = output.counts();
    if found == expected {
        CheckResult::pass(format!("{}, as planned", describe_counts(found)), None)
    } else {
        CheckResult::fail(
            format!(
                "Expected {} but found {}",
                describe_counts(expected),
                describe_counts(found)
            ),
            None,
        )
    }
}

/// "1 video, 2 audio and 1 subtitle tracks".
fn describe_counts(c: StreamSummary) -> String {
    let mut parts = vec![format!("{} video", c.video)];
    if c.audio > 0 || c.subtitle > 0 {
        parts.push(format!("{} audio", c.audio));
    }
    if c.subtitle > 0 {
        parts.push(format!("{} subtitle", c.subtitle));
    }
    let total = c.video + c.audio + c.subtitle;
    let noun = if total == 1 { "track" } else { "tracks" };
    let list = match parts.len() {
        1 => parts.remove(0),
        _ => {
            let last = parts.pop().unwrap_or_default();
            format!("{} and {last}", parts.join(", "))
        }
    };
    format!("{list} {noun}")
}

/// Source and output lengths measured the same way: picture and sound
/// stream durations when both files report them (so a subtitle that runs
/// past the end of the video, and is then dropped, does not count), else
/// container durations.
fn comparable_durations(
    source_info: &ProbeInfo,
    source: Option<&MediaProbe>,
    output: &MediaProbe,
) -> (Option<f64>, Option<f64>) {
    if let (Some(src), Some(out)) = (
        source.and_then(MediaProbe::av_duration),
        output.av_duration(),
    ) {
        return (Some(src), Some(out));
    }
    let src = source_info
        .duration_secs
        .filter(|d| d.is_finite() && *d > 0.0)
        .or_else(|| source.and_then(MediaProbe::best_duration));
    (src, output.best_duration())
}

/// Allowed duration difference: max(1 s, 0.5 %).
fn duration_tolerance(source_secs: f64) -> f64 {
    (source_secs * 0.005).max(1.0)
}

fn duration_check(source: Option<f64>, output: Option<f64>) -> CheckResult {
    let Some(src) = source else {
        return CheckResult::skipped(
            "The original's length is unknown, so it could not be compared",
        );
    };
    let Some(out) = output else {
        return CheckResult::fail("The new file's length could not be read", None);
    };
    let diff = (out - src).abs();
    if diff <= duration_tolerance(src) {
        CheckResult::pass(
            format!(
                "Matches the original ({} vs {})",
                format_time(out),
                format_time(src)
            ),
            Some(diff),
        )
    } else {
        CheckResult::fail(
            format!(
                "The new file is {} long but the original is {}",
                format_time(out),
                format_time(src)
            ),
            Some(diff),
        )
    }
}

fn size_check(source: u64, output: u64) -> CheckResult {
    if source == 0 || output == 0 {
        return CheckResult::skipped("The file sizes could not be read");
    }
    let change = (1.0 - output as f64 / source as f64) * 100.0;
    let sizes = format!("{} → {}", human_bytes(source), human_bytes(output));
    if change >= 0.5 {
        CheckResult::pass(
            format!("{:.0}% smaller than the original ({sizes})", change),
            Some(change),
        )
    } else if change > -0.5 {
        CheckResult::warn(
            format!("About the same size as the original ({sizes})"),
            Some(change),
        )
    } else {
        CheckResult::warn(
            format!("{:.0}% larger than the original ({sizes})", -change),
            Some(change),
        )
    }
}

#[derive(Debug)]
struct DecodeResult {
    exit: FfmpegExit,
    first_error: Option<String>,
    error_count: usize,
    decoded_secs: Option<f64>,
    detect: Option<DetectTotals>,
}

fn decode_check(decode: &DecodeResult, expected_len: Option<f64>) -> CheckResult {
    if let Some(first) = &decode.first_error {
        let more = match decode.error_count {
            0 | 1 => String::new(),
            n => format!(" ({} more)", n - 1),
        };
        return CheckResult::fail(format!("Found a playback error: {first}{more}"), None);
    }
    match &decode.exit {
        FfmpegExit::Success { .. } => {}
        other => {
            let why = other
                .describe_failure("ffmpeg")
                .unwrap_or_else(|| "unknown error".into());
            return CheckResult::fail(
                format!("The new file could not be played through: {why}"),
                None,
            );
        }
    }
    let decoded = decode.decoded_secs;
    if let (Some(expected), Some(got)) = (expected_len, decoded) {
        if got + duration_tolerance(expected) < expected {
            return CheckResult::fail(
                format!(
                    "Playback stopped at {} of {}",
                    format_time(got),
                    format_time(expected)
                ),
                Some(got),
            );
        }
    }
    let length = decoded.or(expected_len).map(format_time);
    CheckResult::pass(
        match length {
            Some(l) => format!("Decoded all {l} without errors"),
            None => "Decoded without errors".to_string(),
        },
        decoded,
    )
}

fn added_time_check(source_secs: f64, output_secs: f64, what: &str) -> CheckResult {
    let added = output_secs - source_secs;
    let detail = format!(
        "{} video: {:.1} s in the original, {:.1} s in the new file",
        capitalize(what),
        source_secs,
        output_secs
    );
    if added > MAX_ADDED_SECS {
        CheckResult::fail(
            format!("The new file has {added:.1} s more {what} video than the original"),
            Some(added),
        )
    } else {
        CheckResult::pass(detail, Some(added.max(0.0)))
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Decode-pass error lines.
#[derive(Debug, Default)]
struct ErrorLines {
    first: Option<String>,
    count: usize,
}

impl ErrorLines {
    /// Count `line` if it is an error. With `untagged_are_errors` (ffmpeg
    /// run at `-loglevel level+error`), lines without a level tag count too.
    fn push(&mut self, line: &str, untagged_are_errors: bool) {
        let (context, level, message) = split_log_line(line);
        let is_error = match level {
            Some(l) => matches!(l, "error" | "fatal" | "panic"),
            None => untagged_are_errors,
        };
        if !is_error || message.trim().is_empty() {
            return;
        }
        self.count += 1;
        if self.first.is_none() {
            let message = message.trim();
            self.first = Some(match context {
                Some(c) => format!("{message} ({c})"),
                None => message.to_string(),
            });
        }
    }
}

/// Totals from `blackdetect` and `freezedetect` log lines.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct DetectTotals {
    black_secs: f64,
    frozen_secs: f64,
    /// A freeze that started but has not ended yet.
    open_freeze: Option<f64>,
}

impl DetectTotals {
    fn push_line(&mut self, line: &str) {
        let (_, _, message) = split_log_line(line);
        if let Some(d) = value_after(message, "black_duration:") {
            self.black_secs += d.max(0.0);
        } else if let Some(start) = value_after(message, "lavfi.freezedetect.freeze_start:") {
            self.open_freeze = Some(start);
        } else if let Some(d) = value_after(message, "lavfi.freezedetect.freeze_duration:") {
            self.frozen_secs += d.max(0.0);
            self.open_freeze = None;
        } else if message.contains("lavfi.freezedetect.freeze_end:") {
            self.open_freeze = None;
        }
    }

    /// Close a freeze still running at the end of the stream (`stream_end` in
    /// the stream's timestamps).
    fn finish(mut self, stream_end: f64) -> Self {
        if let Some(start) = self.open_freeze.take() {
            self.frozen_secs += (stream_end - start).max(0.0);
        }
        self
    }
}

/// Parse the number following `key` in `text`.
fn value_after(text: &str, key: &str) -> Option<f64> {
    let rest = &text[text.find(key)? + key.len()..];
    let token = rest.split_whitespace().next()?;
    token.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Per-frame SSIM "All" values from an `ssim` stats file.
fn parse_ssim_stats(text: &str) -> Vec<f64> {
    text.lines()
        .filter_map(|l| value_after(l, "All:"))
        .collect()
}

/// Per-frame average MSE values from a `psnr` stats file.
fn parse_psnr_stats(text: &str) -> Vec<f64> {
    text.lines()
        .filter_map(|l| value_after(l, "mse_avg:"))
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
struct SegmentStats {
    /// Segment start in the source, seconds from the video start.
    start: f64,
    ssim: Vec<f64>,
    mse: Vec<f64>,
}

#[derive(Debug)]
enum SegmentError {
    Cancelled,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
struct VisualOutcome {
    result: CheckResult,
    ssim_min: Option<f64>,
    ssim_avg: Option<f64>,
    psnr_avg: Option<f64>,
}

impl VisualOutcome {
    fn only(result: CheckResult) -> Self {
        Self {
            result,
            ssim_min: None,
            ssim_avg: None,
            psnr_avg: None,
        }
    }
}

/// Apply the SSIM thresholds to measured segments.
fn summarize_segments(segments: &[SegmentStats]) -> VisualOutcome {
    let frames: Vec<f64> = segments
        .iter()
        .flat_map(|s| s.ssim.iter().copied())
        .collect();
    if frames.is_empty() {
        return VisualOutcome::only(CheckResult::skipped("No pictures were compared"));
    }
    let ssim_avg = frames.iter().sum::<f64>() / frames.len() as f64;
    let ssim_min = frames.iter().copied().fold(f64::INFINITY, f64::min);
    let mses: Vec<f64> = segments
        .iter()
        .flat_map(|s| s.mse.iter().copied())
        .collect();
    let psnr_avg = (!mses.is_empty()).then(|| {
        let mse = mses.iter().sum::<f64>() / mses.len() as f64;
        psnr_from_mse(mse)
    });

    let worst_frame = segments
        .iter()
        .flat_map(|s| s.ssim.iter().map(move |v| (s.start, *v)))
        .min_by(|a, b| a.1.total_cmp(&b.1));
    let worst_segment = segments
        .iter()
        .filter(|s| !s.ssim.is_empty())
        .map(|s| (s.start, s.ssim.iter().sum::<f64>() / s.ssim.len() as f64))
        .min_by(|a, b| a.1.total_cmp(&b.1));

    let result = match (worst_frame, worst_segment) {
        (Some((at, v)), _) if v < FRAME_SSIM_FAIL => CheckResult::fail(
            format!(
                "A frame near {} looks very different from the original (similarity {v:.2})",
                format_time(at)
            ),
            Some(ssim_avg),
        ),
        (_, Some((at, v))) if v < SEGMENT_SSIM_FAIL => CheckResult::fail(
            format!(
                "The picture near {} doesn't match the original (similarity {v:.2})",
                format_time(at)
            ),
            Some(ssim_avg),
        ),
        _ if ssim_avg < OVERALL_SSIM_WARN => CheckResult::warn(
            format!(
                "Noticeably softer than the original (average similarity {ssim_avg:.3} at {} points)",
                segments.len()
            ),
            Some(ssim_avg),
        ),
        _ => CheckResult::pass(
            format!(
                "Matches the original at {} points (average similarity {ssim_avg:.3})",
                segments.len()
            ),
            Some(ssim_avg),
        ),
    };
    VisualOutcome {
        result,
        ssim_min: Some(ssim_min),
        ssim_avg: Some(ssim_avg),
        psnr_avg,
    }
}

/// PSNR in dB for 8-bit pictures, capped for identical pictures.
fn psnr_from_mse(mse: f64) -> f64 {
    if mse <= 0.0 || !mse.is_finite() {
        return PSNR_CAP;
    }
    (10.0 * (255.0f64 * 255.0 / mse).log10()).min(PSNR_CAP)
}

/// `(start, length)` of `count` segments spread evenly over the middle 90 %
/// of the file, each up to [`SEGMENT_SECS`] long.
fn segment_windows(duration: f64, count: usize) -> Vec<(f64, f64)> {
    if !(duration.is_finite() && duration > 0.0) || count == 0 {
        return Vec::new();
    }
    let lo = duration * EDGE_FRACTION;
    let hi = duration * (1.0 - EDGE_FRACTION);
    let slice = (hi - lo) / count as f64;
    let len = slice.min(SEGMENT_SECS);
    if len < 0.2 {
        return Vec::new();
    }
    (0..count)
        .map(|i| (lo + slice * i as f64 + (slice - len) / 2.0, len))
        .collect()
}

// ---------------------------------------------------------------------------
// Segment comparison command

/// How to line up source and output for a picture comparison.
///
/// Comparisons run with `-copyts`, so frame timestamps inside the filter
/// graph are each file's own timestamps. Each side is seeked a little early
/// and then cut with `trim` at `video start + t`, which is exact even when
/// seeking is not (MPEG-TS, AVI without index).
#[derive(Debug, Clone, PartialEq)]
struct Alignment {
    source_index: u32,
    output_index: u32,
    /// Timestamp of the first video frame, per file.
    source_video_start: f64,
    output_video_start: f64,
    /// Container start time, per file (`-ss` is relative to it).
    source_format_start: f64,
    output_format_start: f64,
    width: u32,
    height: u32,
    /// Frame grid both sides are put on (`fps=` expression).
    rate_expr: String,
    /// Source frame rate, for picking seek points between frames.
    source_fps: f64,
    /// `bwdif` mode for the source when the output was deinterlaced.
    deinterlace: Option<&'static str>,
}

impl Alignment {
    fn new(
        source_info: &ProbeInfo,
        source: Option<&MediaProbe>,
        output: &MediaProbe,
    ) -> Option<Self> {
        let out_video = output.primary_video()?;
        let (width, height) = (out_video.width?, out_video.height?);
        let src_video = source.and_then(MediaProbe::primary_video);
        let source_index = src_video
            .map(|v| v.index)
            .or_else(|| source_info.primary_video().map(|v| v.index))?;

        let source_format_start = source
            .and_then(|s| s.start_time)
            .or(source_info.start_time)
            .unwrap_or(0.0);
        let source_video_start = src_video
            .and_then(|v| v.start_time)
            .unwrap_or(source_format_start);
        let output_format_start = output.start_time.unwrap_or(0.0);
        let output_video_start = out_video.start_time.unwrap_or(output_format_start);

        let source_fps = src_video
            .and_then(|v| v.frame_rate.as_ref().map(Rational::value))
            .or_else(|| source_info.primary_video().and_then(|v| v.frame_rate))
            .filter(|f| sane_fps(*f));
        let out_rate = out_video.frame_rate.filter(|r| sane_fps(r.value()));
        let rate_expr = out_rate
            .as_ref()
            .map(Rational::expr)
            .or_else(|| source_fps.map(|f| format!("{f:.6}")))
            .unwrap_or_else(|| "25".to_string());
        let out_fps = out_rate.as_ref().map(Rational::value);

        let source_interlaced = src_video.is_some_and(|v| v.interlaced)
            || source_info.primary_video().is_some_and(|v| v.interlaced);
        // Field-rate output (50p from 50i fields) needs a field-rate reference.
        let mode = match (out_fps, source_fps) {
            (Some(o), Some(s)) if o > s * 1.5 => "send_field",
            _ => "send_frame",
        };
        let deinterlace = (source_interlaced && !out_video.interlaced).then_some(mode);
        Some(Self {
            source_index,
            output_index: out_video.index,
            source_video_start,
            output_video_start,
            source_format_start,
            output_format_start,
            width,
            height,
            rate_expr,
            source_fps: source_fps.or(out_fps).unwrap_or(25.0),
            deinterlace,
        })
    }

    /// A point near `t` that falls a quarter frame before a source frame, so
    /// both files start on the same picture even when one of them rounds
    /// timestamps (Matroska stores milliseconds) or doubles the frame rate
    /// (field-rate deinterlacing).
    fn seek_point(&self, t: f64) -> f64 {
        let fps = self.source_fps;
        (((t * fps).round() - 0.25) / fps).max(0.0)
    }
}

fn sane_fps(f: f64) -> bool {
    f.is_finite() && (1.0..=300.0).contains(&f)
}

/// One comparison window, `at..at + len` seconds after the video start.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SegmentWindow {
    at: f64,
    len: f64,
    /// Seek this many seconds early in the source (the output, which we
    /// wrote with an index, gets at most a second). `None` reads both files
    /// from the start.
    source_preroll: Option<f64>,
}

/// Input options that read `window` from one file: `-ss`/`-t` relative to
/// the container start, reading from `preroll` seconds early.
fn input_window(
    video_start: f64,
    format_start: f64,
    window: &SegmentWindow,
    preroll: Option<f64>,
) -> Vec<String> {
    let offset = video_start - format_start;
    let end = offset + window.at + window.len;
    let seek = preroll.map_or(0.0, |p| (offset + window.at - p).max(0.0));
    let mut args = Vec::with_capacity(4);
    if seek > 0.0 {
        args.extend(["-ss".to_string(), format!("{seek:.6}")]);
    }
    args.extend(["-t".to_string(), format!("{:.6}", end - seek + 1.0)]);
    args
}

fn segment_args(
    req: &ValidateRequest<'_>,
    align: &Alignment,
    window: &SegmentWindow,
    ssim_path: &Path,
    psnr_path: &Path,
) -> Vec<String> {
    let deinterlace = align
        .deinterlace
        .map(|mode| format!("bwdif=mode={mode},"))
        .unwrap_or_default();
    let trim = |video_start: f64| {
        let start = video_start + window.at;
        format!("trim=start={start:.6}:end={:.6}", start + window.len)
    };
    let graph = format!(
        "[1:{oi}]{out_trim},setpts=PTS-STARTPTS,fps={rate},format=yuv420p,split=2[d0][d1];\
         [0:{si}]{deinterlace}{src_trim},setpts=PTS-STARTPTS,fps={rate},\
         scale={w}:{h}:flags=bicubic,format=yuv420p,split=2[r0][r1];\
         [d0][r0]ssim=stats_file={ssim}:shortest=1[vs];\
         [d1][r1]psnr=stats_file={psnr}:shortest=1[vp]",
        oi = align.output_index,
        si = align.source_index,
        out_trim = trim(align.output_video_start),
        src_trim = trim(align.source_video_start),
        rate = align.rate_expr,
        w = align.width,
        h = align.height,
        ssim = escape_filter_value(&ssim_path.to_string_lossy()),
        psnr = escape_filter_value(&psnr_path.to_string_lossy()),
    );
    let output_preroll = window.source_preroll.map(|p| p.min(1.0));
    // `-progress` keeps output flowing on slow machines so the stall watchdog
    // only fires for a real hang.
    let mut args: Vec<String> = [
        "-nostdin",
        "-hide_banner",
        "-v",
        "error",
        "-progress",
        "pipe:1",
        "-nostats",
        "-copyts",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.extend(input_window(
        align.source_video_start,
        align.source_format_start,
        window,
        window.source_preroll,
    ));
    args.extend(["-i".to_string(), req.source.to_string_lossy().into_owned()]);
    args.extend(input_window(
        align.output_video_start,
        align.output_format_start,
        window,
        output_preroll,
    ));
    args.extend(["-i".to_string(), req.output.to_string_lossy().into_owned()]);
    args.extend(
        [
            "-filter_complex",
            &graph,
            "-map",
            "[vs]",
            "-map",
            "[vp]",
            "-f",
            "null",
            "-",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args
}

/// Escape a value for use as a filter option inside a filtergraph (both
/// escaping levels described in ffmpeg-filters "Notes on filtergraph
/// escaping").
fn escape_filter_value(value: &str) -> String {
    let mut option_level = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\\' | '\'' | ':') {
            option_level.push('\\');
        }
        option_level.push(c);
    }
    let mut graph_level = String::with_capacity(option_level.len());
    for c in option_level.chars() {
        if matches!(c, '\\' | '\'' | '[' | ']' | ',' | ';') {
            graph_level.push('\\');
        }
        graph_level.push(c);
    }
    graph_level
}

async fn make_stats_dir() -> std::io::Result<tempfile::TempDir> {
    tokio::task::spawn_blocking(|| {
        tempfile::Builder::new()
            .prefix("chrysopoeia-verify-")
            .tempdir()
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Delete the scratch directory off the async threads.
async fn discard_dir(dir: tempfile::TempDir) {
    if let Err(e) = tokio::task::spawn_blocking(move || dir.close()).await {
        tracing::debug!("could not remove verification scratch files: {e}");
    }
}

// ---------------------------------------------------------------------------
// ffprobe

#[derive(Debug)]
enum ProbeError {
    Cancelled,
    Failed(String),
}

/// What verification needs to know about a file.
#[derive(Debug, Clone, Default, PartialEq)]
struct MediaProbe {
    duration: Option<f64>,
    start_time: Option<f64>,
    streams: Vec<ProbedStream>,
}

#[derive(Debug, Clone, Default, PartialEq)]
struct ProbedStream {
    index: u32,
    kind: Option<StreamKind>,
    codec: String,
    attached_pic: bool,
    width: Option<u32>,
    height: Option<u32>,
    interlaced: bool,
    frame_rate: Option<Rational>,
    start_time: Option<f64>,
    duration: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rational {
    num: u64,
    den: u64,
}

impl Rational {
    fn parse(s: &str) -> Option<Self> {
        let (n, d) = s.split_once('/')?;
        let (num, den) = (n.trim().parse().ok()?, d.trim().parse().ok()?);
        (num > 0 && den > 0).then_some(Self { num, den })
    }

    fn value(&self) -> f64 {
        self.num as f64 / self.den as f64
    }

    fn expr(&self) -> String {
        format!("{}/{}", self.num, self.den)
    }
}

impl MediaProbe {
    fn primary_video(&self) -> Option<&ProbedStream> {
        self.streams
            .iter()
            .find(|s| s.kind == Some(StreamKind::Video) && !s.attached_pic)
    }

    fn counts(&self) -> StreamSummary {
        let count = |kind| {
            self.streams
                .iter()
                .filter(|s| s.kind == Some(kind) && !s.attached_pic)
                .count() as u32
        };
        StreamSummary {
            video: count(StreamKind::Video),
            audio: count(StreamKind::Audio),
            subtitle: count(StreamKind::Subtitle),
        }
    }

    /// Longest picture or sound stream, when the streams report durations.
    /// Unlike the container duration, this ignores subtitles that run past
    /// the end of the video.
    fn av_duration(&self) -> Option<f64> {
        self.streams
            .iter()
            .filter(|s| {
                !s.attached_pic && matches!(s.kind, Some(StreamKind::Video | StreamKind::Audio))
            })
            .filter_map(|s| s.duration)
            .filter(|d| d.is_finite() && *d > 0.0)
            .reduce(f64::max)
    }

    /// Container duration, else the longest stream.
    fn best_duration(&self) -> Option<f64> {
        self.duration.filter(|d| *d > 0.0).or_else(|| {
            self.streams
                .iter()
                .filter_map(|s| s.duration)
                .filter(|d| d.is_finite() && *d > 0.0)
                .reduce(f64::max)
        })
    }
}

#[derive(Debug, Deserialize)]
struct FfprobeJson {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    format: Option<FfprobeFormat>,
}

#[derive(Debug, Deserialize)]
struct FfprobeStream {
    index: u32,
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    field_order: Option<String>,
    avg_frame_rate: Option<String>,
    r_frame_rate: Option<String>,
    start_time: Option<String>,
    duration: Option<String>,
    disposition: Option<FfprobeDisposition>,
    /// Matroska keeps per-stream durations in a `DURATION` tag.
    #[serde(default)]
    tags: std::collections::HashMap<String, String>,
}

impl FfprobeStream {
    fn duration_secs(&self) -> Option<f64> {
        parse_seconds(self.duration.as_ref()).or_else(|| {
            self.tags
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("duration") || k.starts_with("DURATION-"))
                .and_then(|(_, v)| crate::ffmpeg::parse_clock(v))
        })
    }
}

#[derive(Debug, Deserialize)]
struct FfprobeDisposition {
    #[serde(default)]
    attached_pic: i64,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
    start_time: Option<String>,
}

fn parse_seconds(value: Option<&String>) -> Option<f64> {
    value?.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Parse `ffprobe -print_format json -show_format -show_streams` output.
fn parse_probe_json(json: &[u8]) -> Result<MediaProbe, String> {
    let raw: FfprobeJson = serde_json::from_slice(json)
        .map_err(|e| format!("ffprobe gave an unreadable answer ({e})"))?;
    let streams = raw
        .streams
        .into_iter()
        .map(|s| {
            let kind = match s.codec_type.as_deref() {
                Some("video") => Some(StreamKind::Video),
                Some("audio") => Some(StreamKind::Audio),
                Some("subtitle") => Some(StreamKind::Subtitle),
                Some("attachment") => Some(StreamKind::Attachment),
                Some("data") => Some(StreamKind::Data),
                _ => None,
            };
            let frame_rate = s
                .avg_frame_rate
                .as_deref()
                .and_then(Rational::parse)
                .filter(|r| sane_fps(r.value()))
                .or_else(|| s.r_frame_rate.as_deref().and_then(Rational::parse));
            let duration = s.duration_secs();
            ProbedStream {
                duration,
                index: s.index,
                kind,
                codec: s.codec_name.unwrap_or_default(),
                attached_pic: s.disposition.is_some_and(|d| d.attached_pic != 0),
                width: s.width.filter(|w| *w > 0),
                height: s.height.filter(|h| *h > 0),
                interlaced: matches!(s.field_order.as_deref(), Some("tt" | "bb" | "tb" | "bt")),
                frame_rate,
                start_time: parse_seconds(s.start_time.as_ref()),
            }
        })
        .collect();
    let format = raw.format;
    Ok(MediaProbe {
        duration: format
            .as_ref()
            .and_then(|f| parse_seconds(f.duration.as_ref())),
        start_time: format
            .as_ref()
            .and_then(|f| parse_seconds(f.start_time.as_ref())),
        streams,
    })
}

/// Run ffprobe on `path` (killed after [`PROBE_TIMEOUT`] or on cancel).
async fn probe_media(
    ffprobe: &Path,
    path: &Path,
    cancel: &CancellationToken,
) -> Result<MediaProbe, ProbeError> {
    let mut command = tokio::process::Command::new(ffprobe);
    command
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command.spawn().map_err(|e| {
        ProbeError::Failed(if e.kind() == std::io::ErrorKind::NotFound {
            format!("ffprobe was not found at {}", ffprobe.display())
        } else {
            format!("ffprobe could not start ({e})")
        })
    })?;
    let output = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProbeError::Cancelled),
        result = tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output()) => match result {
            Err(_) => return Err(ProbeError::Failed(format!(
                "ffprobe gave no answer within {} seconds", PROBE_TIMEOUT.as_secs()
            ))),
            Ok(Err(e)) => return Err(ProbeError::Failed(format!("ffprobe failed ({e})"))),
            Ok(Ok(output)) => output,
        },
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .lines()
            .map(|l| crate::ffmpeg::strip_log_prefix(l).trim())
            .find(|l| !l.is_empty())
            .unwrap_or("it is not a readable media file")
            .to_string();
        return Err(ProbeError::Failed(reason));
    }
    parse_probe_json(&output.stdout).map_err(ProbeError::Failed)
}

// ---------------------------------------------------------------------------
// Formatting

/// `6.0 s`, `4:05` or `1:42:10`.
fn format_time(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "0.0 s".to_string();
    }
    if secs < 60.0 {
        return format!("{secs:.1} s");
    }
    let total = secs.round() as u64;
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Decimal size with one decimal place, e.g. `12.3 GB`.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// SSIM and PSNR statistics files for one segment.
fn stats_paths(dir: &Path, index: usize) -> (PathBuf, PathBuf) {
    (
        dir.join(format!("seg{index}_ssim.log")),
        dir.join(format!("seg{index}_psnr.log")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ssim_and_psnr_stats() {
        let ssim = "n:1 Y:0.994541 U:0.994954 V:0.996288 All:0.994901 (22.925284)\n\
                    n:2 Y:1.000000 U:1.000000 V:1.000000 All:1.000000 (inf)\n";
        assert_eq!(parse_ssim_stats(ssim), [0.994901, 1.0]);
        let psnr = "n:1 mse_avg:2.09 mse_y:1.71 mse_u:2.89 mse_v:2.82 psnr_avg:44.92 psnr_y:45.79 \n\
                    n:2 mse_avg:0.00 mse_y:0.00 mse_u:0.00 mse_v:0.00 psnr_avg:inf psnr_y:inf \n";
        assert_eq!(parse_psnr_stats(psnr), [2.09, 0.0]);
        assert!((psnr_from_mse(2.09) - 44.93).abs() < 0.01);
        assert_eq!(psnr_from_mse(0.0), PSNR_CAP);
    }

    #[test]
    fn detect_totals_from_log_lines() {
        let lines = [
            "[blackdetect @ 0x7f10d4000e40] [info] black_start:6 black_end:10.958 black_duration:4.958",
            "[freezedetect @ 0x55] [info] lavfi.freezedetect.freeze_start: 3.065",
            "[freezedetect @ 0x55] [info] lavfi.freezedetect.freeze_duration: 2",
            "[freezedetect @ 0x55] [info] lavfi.freezedetect.freeze_end: 5.065",
            "[freezedetect @ 0x55] [info] lavfi.freezedetect.freeze_start: 9",
            "[info] unrelated line",
        ];
        let mut totals = DetectTotals::default();
        for l in lines {
            totals.push_line(l);
        }
        let totals = totals.finish(11.0);
        assert!((totals.black_secs - 4.958).abs() < 1e-9);
        // 2 s closed freeze + 2 s still frozen at the end of the stream.
        assert!((totals.frozen_secs - 4.0).abs() < 1e-9);
    }

    #[test]
    fn error_lines_respect_levels() {
        let mut errors = ErrorLines::default();
        errors.push("[h264 @ 0x56] [info] concealing 3253 DC errors", false);
        errors.push(
            "[vist#0:0/h264 @ 0x56] [warning] corrupt decoded frame",
            false,
        );
        assert_eq!(errors.count, 0);
        errors.push("[h264 @ 0x56] [error] cbp too large (353) at 76 4", false);
        errors.push("[h264 @ 0x56] [error] error while decoding MB 76 4", false);
        assert_eq!(errors.count, 2);
        assert_eq!(
            errors.first.as_deref(),
            Some("cbp too large (353) at 76 4 (h264)")
        );

        let mut strict = ErrorLines::default();
        strict.push("Invalid data found when processing input", true);
        assert_eq!(strict.count, 1);
    }

    #[test]
    fn describes_stream_counts() {
        let c = |video, audio, subtitle| StreamSummary {
            video,
            audio,
            subtitle,
        };
        assert_eq!(describe_counts(c(1, 0, 0)), "1 video track");
        assert_eq!(describe_counts(c(1, 1, 0)), "1 video and 1 audio tracks");
        assert_eq!(
            describe_counts(c(1, 2, 1)),
            "1 video, 2 audio and 1 subtitle tracks"
        );
    }

    #[test]
    fn duration_tolerance_is_one_second_or_half_a_percent() {
        assert!(duration_check(Some(6.0), Some(6.9)).status == CheckStatus::Pass);
        assert!(duration_check(Some(6.0), Some(3.0)).status == CheckStatus::Fail);
        // 2 h: 0.5 % is 36 s.
        assert!(duration_check(Some(7200.0), Some(7170.0)).status == CheckStatus::Pass);
        assert!(duration_check(Some(7200.0), Some(7150.0)).status == CheckStatus::Fail);
        assert_eq!(duration_check(None, Some(1.0)).status, CheckStatus::Skipped);
        assert_eq!(duration_check(Some(1.0), None).status, CheckStatus::Fail);
    }

    #[test]
    fn segments_avoid_the_edges() {
        let segs = segment_windows(100.0, 4);
        assert_eq!(segs.len(), 4);
        for (start, len) in &segs {
            assert!(*start >= 5.0 && start + len <= 95.0);
            assert_eq!(*len, SEGMENT_SECS);
        }
        // Short files get shorter segments; tiny files none.
        let short = segment_windows(6.0, 10);
        assert!(short.iter().all(|(_, len)| *len < 0.6));
        assert!(segment_windows(0.5, 10).is_empty());
    }

    #[test]
    fn ssim_thresholds() {
        let seg = |start, ssim: Vec<f64>| SegmentStats {
            start,
            mse: vec![1.0; ssim.len()],
            ssim,
        };
        let good = summarize_segments(&[seg(1.0, vec![0.99, 0.98]), seg(5.0, vec![0.97])]);
        assert_eq!(good.result.status, CheckStatus::Pass);
        assert_eq!(good.ssim_min, Some(0.97));

        let corrupt_frame = summarize_segments(&[seg(1.0, vec![0.99, 0.55, 0.99])]);
        assert_eq!(corrupt_frame.result.status, CheckStatus::Fail);

        let bad_segment = summarize_segments(&[seg(1.0, vec![0.99]), seg(3.0, vec![0.8, 0.82])]);
        assert_eq!(bad_segment.result.status, CheckStatus::Fail);
        assert!(bad_segment.result.detail.contains("3.0 s"));

        let soft = summarize_segments(&[seg(1.0, vec![0.90, 0.91])]);
        assert_eq!(soft.result.status, CheckStatus::Warn);
    }

    #[test]
    fn seek_points_fall_between_frames() {
        let align = Alignment {
            source_index: 0,
            output_index: 0,
            source_video_start: 0.0,
            output_video_start: 0.0,
            source_format_start: 0.0,
            output_format_start: 0.0,
            width: 2,
            height: 2,
            rate_expr: "24/1".into(),
            source_fps: 24.0,
            deinterlace: None,
        };
        let at = align.seek_point(1.35);
        // 1.35 s ≈ frame 32; seek a quarter frame before it.
        assert!((at - (32.0 - 0.25) / 24.0).abs() < 1e-9);
        assert_eq!(align.seek_point(0.0), 0.0);
    }

    #[test]
    fn escapes_filter_values() {
        assert_eq!(escape_filter_value("/tmp/a.log"), "/tmp/a.log");
        // Verified against ffmpeg: a stats file in "weird:dir,with;[brackets]'q"
        // lands in that folder with this escaping.
        assert_eq!(
            escape_filter_value("weird:dir,with;[brackets]'q/s.log"),
            r"weird\\:dir\,with\;\[brackets\]\\\'q/s.log"
        );
        assert_eq!(escape_filter_value(r"C:\x"), r"C\\:\\\\x");
    }

    #[test]
    fn parses_probe_json() {
        let json = br#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"mjpeg","width":300,"height":300,
             "disposition":{"attached_pic":1}},
            {"index":1,"codec_type":"video","codec_name":"hevc","width":1920,"height":1080,
             "field_order":"progressive","avg_frame_rate":"24000/1001","r_frame_rate":"24000/1001",
             "start_time":"0.042000","disposition":{"attached_pic":0}},
            {"index":2,"codec_type":"audio","codec_name":"opus","start_time":"-0.007000"},
            {"index":3,"codec_type":"subtitle","codec_name":"subrip"}],
            "format":{"duration":"6.042000","start_time":"-0.007000"}}"#;
        let probe = parse_probe_json(json).unwrap();
        let video = probe.primary_video().unwrap();
        assert_eq!(video.index, 1);
        assert_eq!(video.frame_rate.unwrap().expr(), "24000/1001");
        assert_eq!(
            probe.counts(),
            StreamSummary {
                video: 1,
                audio: 1,
                subtitle: 1
            }
        );
        assert_eq!(probe.best_duration(), Some(6.042));
        assert!(parse_probe_json(b"not json").is_err());

        let output = probe.clone();
        let align = Alignment::new(&ProbeInfo::default(), Some(&probe), &output).unwrap();
        assert_eq!(align.source_video_start, 0.042);
        assert_eq!(align.source_format_start, -0.007);
        assert_eq!(align.rate_expr, "24000/1001");
        assert_eq!(align.deinterlace, None);
    }

    #[test]
    fn stream_durations_ignore_long_subtitles() {
        // Matroska: durations only in tags; the subtitle outlasts the video.
        let json = br#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","width":64,"height":64,
             "tags":{"DURATION":"00:00:04.000000000"}},
            {"index":1,"codec_type":"audio","codec_name":"ac3",
             "tags":{"DURATION-eng":"00:00:04.032000000"}},
            {"index":2,"codec_type":"subtitle","codec_name":"subrip",
             "tags":{"DURATION":"00:00:05.000000000"}}],
            "format":{"duration":"5.000000"}}"#;
        let source = parse_probe_json(json).unwrap();
        assert_eq!(source.primary_video().unwrap().duration, Some(4.0));
        assert_eq!(source.av_duration(), Some(4.032));
        assert_eq!(source.best_duration(), Some(5.0));

        // Output without the subtitle: 4.03 s container, same A/V length.
        let out_json = br#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","duration":"4.000000"},
            {"index":1,"codec_type":"audio","codec_name":"aac","duration":"4.030000"}],
            "format":{"duration":"4.030000"}}"#;
        let output = parse_probe_json(out_json).unwrap();
        let info = ProbeInfo {
            duration_secs: Some(5.0),
            ..Default::default()
        };
        assert_eq!(
            comparable_durations(&info, Some(&source), &output),
            (Some(4.032), Some(4.03))
        );
        // Without stream durations on one side, containers are compared.
        assert_eq!(
            comparable_durations(&info, None, &output),
            (Some(5.0), Some(4.03))
        );
    }

    #[test]
    fn input_windows_seek_early_and_read_past_the_end() {
        let window = SegmentWindow {
            at: 10.0,
            len: 2.0,
            source_preroll: Some(5.0),
        };
        // MPEG-TS style: video starts 0.011 s after the container.
        let args = input_window(1.483, 1.472, &window, Some(5.0));
        assert_eq!(args, ["-ss", "5.011000", "-t", "8.000000"]);
        // Near the start there is nothing to skip.
        let early = SegmentWindow { at: 2.0, ..window };
        assert_eq!(
            input_window(0.0, 0.0, &early, Some(5.0)),
            ["-t", "5.000000"]
        );
        // No preroll: read from the start.
        assert_eq!(
            input_window(0.0, -0.023, &window, None),
            ["-t", "13.023000"]
        );
    }

    #[test]
    fn segment_commands_trim_on_absolute_timestamps() {
        let probe = ProbeInfo::default();
        let profile = TranscodeProfile::default();
        let req = ValidateRequest {
            ffmpeg: Path::new("ffmpeg"),
            ffprobe: Path::new("ffprobe"),
            source: Path::new("/m/in.ts"),
            source_probe: &probe,
            output: Path::new("/t/out.mkv"),
            profile: &profile,
            level: ValidationLevel::Standard,
            expected: StreamSummary::default(),
        };
        let align = Alignment {
            source_index: 0,
            output_index: 0,
            source_video_start: 1.5,
            output_video_start: 0.0,
            source_format_start: 1.4,
            output_format_start: -0.023,
            width: 720,
            height: 576,
            rate_expr: "25/1".into(),
            source_fps: 25.0,
            deinterlace: Some("send_frame"),
        };
        let window = SegmentWindow {
            at: 10.0,
            len: 2.0,
            source_preroll: Some(SEEK_PREROLL_SECS),
        };
        let args = segment_args(
            &req,
            &align,
            &window,
            Path::new("/tmp/s.log"),
            Path::new("/tmp/p.log"),
        );
        assert!(args.contains(&"-copyts".to_string()));
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        assert!(graph.contains("[1:0]trim=start=10.000000:end=12.000000,setpts=PTS-STARTPTS"));
        assert!(graph.contains("[0:0]bwdif=mode=send_frame,trim=start=11.500000:end=13.500000"));
        assert!(graph.contains("scale=720:576"));
        assert!(graph.contains("ssim=stats_file=/tmp/s.log:shortest=1"));
    }

    #[test]
    fn interlaced_sources_are_deinterlaced_to_match() {
        let stream = |interlaced, rate: &str| ProbedStream {
            index: 0,
            kind: Some(StreamKind::Video),
            codec: "h264".into(),
            width: Some(720),
            height: Some(576),
            interlaced,
            frame_rate: Rational::parse(rate),
            ..Default::default()
        };
        let probe = |s| MediaProbe {
            streams: vec![s],
            ..Default::default()
        };
        let source = probe(stream(true, "25/1"));
        let frame_rate = Alignment::new(
            &ProbeInfo::default(),
            Some(&source),
            &probe(stream(false, "25/1")),
        );
        assert_eq!(frame_rate.unwrap().deinterlace, Some("send_frame"));
        let field_rate = Alignment::new(
            &ProbeInfo::default(),
            Some(&source),
            &probe(stream(false, "50/1")),
        );
        assert_eq!(field_rate.unwrap().deinterlace, Some("send_field"));
        let kept = Alignment::new(
            &ProbeInfo::default(),
            Some(&source),
            &probe(stream(true, "25/1")),
        );
        assert_eq!(kept.unwrap().deinterlace, None);
    }

    #[test]
    fn formats_times_and_sizes() {
        assert_eq!(format_time(6.04), "6.0 s");
        assert_eq!(format_time(245.0), "4:05");
        assert_eq!(format_time(6130.0), "1:42:10");
        assert_eq!(human_bytes(512), "512 bytes");
        assert_eq!(human_bytes(12_345_678_901), "12.3 GB");
    }

    #[test]
    fn stats_paths_are_per_segment() {
        let (s, p) = stats_paths(Path::new("/tmp/x"), 3);
        assert_eq!(s, Path::new("/tmp/x/seg3_ssim.log"));
        assert_eq!(p, Path::new("/tmp/x/seg3_psnr.log"));
    }

    #[tokio::test]
    async fn off_level_reports_nothing() {
        let probe = ProbeInfo::default();
        let profile = TranscodeProfile::default();
        let req = ValidateRequest {
            ffmpeg: Path::new("ffmpeg"),
            ffprobe: Path::new("ffprobe"),
            source: Path::new("/nonexistent/in.mkv"),
            source_probe: &probe,
            output: Path::new("/nonexistent/out.mkv"),
            profile: &profile,
            level: ValidationLevel::Off,
            expected: StreamSummary::default(),
        };
        let report = validate_output(&req, &CancellationToken::new(), &|_| {}).await;
        assert!(report.passed);
        assert!(report.checks.is_empty());
    }

    #[tokio::test]
    async fn unreadable_output_fails_probe_and_skips_the_rest() {
        let probe = ProbeInfo::default();
        let profile = TranscodeProfile::default();
        let req = ValidateRequest {
            ffmpeg: Path::new("ffmpeg"),
            ffprobe: Path::new("/nonexistent/ffprobe"),
            source: Path::new("/nonexistent/in.mkv"),
            source_probe: &probe,
            output: Path::new("/nonexistent/out.mkv"),
            profile: &profile,
            level: ValidationLevel::Standard,
            expected: StreamSummary::default(),
        };
        let report = validate_output(&req, &CancellationToken::new(), &|_| {}).await;
        assert!(!report.passed);
        assert_eq!(report.checks.len(), 6);
        assert_eq!(report.checks[0].status, CheckStatus::Fail);
        assert_eq!(report.checks[0].label, "Opens correctly");
        assert!(
            report.checks[1..]
                .iter()
                .all(|c| c.status == CheckStatus::Skipped)
        );
    }
}
