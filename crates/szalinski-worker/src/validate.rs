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
//! **Same length as the original** compares the picture and sound lengths
//! of both files, counted from where each file starts. A stream's length is
//! ffprobe's own, else its Matroska `DURATION` tag (the plain one before a
//! localized `DURATION-eng`, which an ffmpeg cut leaves over from the film
//! it came from). A tag the container's own length contradicts, tags that
//! are a file's only length, and the lengths of a Matroska file whose
//! timestamps don't start at zero (ffmpeg's writer states where it ends)
//! are checked against where the streams' last packets end, and a file that
//! states no length at all is measured by them (see `probe_media`). When
//! the original's length can't be read at all, the check fails: nothing
//! could tell a complete new file from one cut short. The decode below
//! must reach the new file's length read the same way.
//!
//! **Plays start to finish** decodes every picture and sound track. Any
//! error, a "corrupt decoded frame" warning or an error-concealment notice
//! counts as damage. Damage in a track that is the same codec in the
//! original is compared with a decode of the original: if the original has
//! the same damage (a copied track from an old recording), the check warns
//! instead of failing.
//!
//! **Looks like the original** compares short segments (4 at standard, 10
//! at thorough). Each segment is decoded from both files, the original is
//! scaled to the new file's size (and deinterlaced with `bwdif` when the new
//! file was), and both are reduced to about 640x360 before SSIM and PSNR are
//! measured. The reduction averages film grain away (encoders remove grain
//! by design, which full-resolution SSIM punishes) while green or grey
//! frames, heavy blocking and wrong content still score very low. The new
//! file is aligned on the shared timeline ffmpeg preserves (so frames padded
//! in at the start of an MP4 do not shift the comparison) and the best match
//! within a few frames is used. At thorough level every frame of both files
//! is also compared at about 320x180, which catches short bursts of
//! corruption between the segments, and black and frozen video are measured
//! on the same reduced, lightly blurred pictures so grain cannot make a
//! still shot look "moving" in the original only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::Deserialize;
use szalinski_core::timeline::{self, PACKET_ENTRIES, PacketEnds};
use szalinski_core::{
    CheckStatus, Container, ProbeInfo, StreamKind, TranscodeProfile, ValidationCheck,
    ValidationLevel, ValidationReport, VideoCodec,
};
use tokio::io::AsyncBufReadExt as _;
use tokio_util::sync::CancellationToken;

use crate::ffmpeg::{
    FfmpegCommand, FfmpegExit, ProgressBlock, compute_progress, run_ffmpeg, split_log_line,
};
use crate::plan::StreamSummary;

/// A frame below this SSIM means corruption.
pub const FRAME_SSIM_FAIL: f64 = 0.60;
/// A segment (or, in the full-length scan, any second) whose mean SSIM is
/// below this does not match the source.
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
/// Pictures are reduced to about this many pixels (640x360) for the segment
/// comparison.
const COMPARE_PIXELS: f64 = 640.0 * 360.0;
/// Pictures are reduced to about this many pixels (320x180) for the
/// full-length scan at thorough level.
const SCAN_PIXELS: f64 = 320.0 * 180.0;
/// Frames searched either side of the expected alignment in each segment.
const ALIGN_SEARCH_FRAMES: i64 = 3;
/// Frames searched either side in the full-length scan (local drift and
/// frames that land either side of a scene cut).
const SCAN_SEARCH_FRAMES: i64 = 1;
/// In the full-length scan, this many consecutive frames below
/// [`FRAME_SSIM_FAIL`] fail the check.
const SCAN_MIN_BAD_FRAMES: usize = 3;
/// In the full-length scan, frames this far below the file's typical
/// similarity for [`SCAN_ANOMALY_SECS`] or longer are a burst of corruption.
const SCAN_ANOMALY_DROP: f64 = 0.10;
/// See [`SCAN_ANOMALY_DROP`].
const SCAN_ANOMALY_SECS: f64 = 0.5;
/// Black and frozen intervals of the original are widened by this much
/// before they are subtracted from the new file's (boundaries jitter).
const INTERVAL_SLACK_SECS: f64 = 0.25;
/// ffprobe gets this long to describe a file, and a packet listing may go
/// this long without a new packet before it is given up.
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const DECODE_STALL: Duration = Duration::from_secs(600);
const SEGMENT_STALL: Duration = Duration::from_secs(180);
/// `blackdetect` options: at least 0.5 s where 90 % of the picture is near
/// black.
const BLACKDETECT_OPTS: &str = "d=0.5:pix_th=0.10";
/// Blur applied before `freezedetect` (on the reduced picture).
const FREEZE_BLUR: &str = "gblur=sigma=1";
/// `freezedetect` options: at least 0.5 s where the reduced, blurred
/// picture changes by less than about 0.6 % between frames. Measured the
/// same way on both files, so grain the encoder removed does not turn a
/// still shot into a freeze in the new file only.
const FREEZEDETECT_OPTS: &str = "n=-45dB:d=0.5";
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
    scan: (f32, f32),
}

impl Weights {
    fn for_level(level: ValidationLevel) -> Self {
        match level {
            ValidationLevel::Off | ValidationLevel::Quick => Self {
                probe: (0.0, 100.0),
                decode: (100.0, 100.0),
                visual: (100.0, 100.0),
                scan: (100.0, 100.0),
            },
            ValidationLevel::Standard => Self {
                probe: (0.0, 5.0),
                decode: (5.0, 75.0),
                visual: (75.0, 100.0),
                scan: (100.0, 100.0),
            },
            ValidationLevel::Thorough => Self {
                probe: (0.0, 2.0),
                decode: (2.0, 35.0),
                visual: (35.0, 45.0),
                scan: (45.0, 100.0),
            },
        }
    }
}

/// Split a phase's share of the progress bar: the first `fraction` of it,
/// and the rest.
fn split_weight((start, end): (f32, f32), fraction: f32) -> ((f32, f32), (f32, f32)) {
    let mid = start + (end - start) * fraction;
    ((start, mid), (mid, end))
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

impl<'a> Ctx<'_, 'a> {
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

    fn command<'c>(&self, args: &'c [String], stall_timeout: Duration) -> FfmpegCommand<'c>
    where
        'a: 'c,
    {
        FfmpegCommand {
            program: self.req.ffmpeg,
            args,
            low_priority: self.low_priority,
            stall_timeout,
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

        // Same length as the original. An original whose length can't be
        // read at all (it states none that can be trusted and its packets
        // couldn't be listed, or it states none and is cut off) leaves
        // nothing to tell a complete new file from one cut short, so it
        // fails rather than pass unchecked.
        let length = match source.as_ref() {
            Some(src) if src.length_unknown => {
                CheckResult::fail(unread_source_length(src.cut_off), None)
            }
            _ => duration_check(source_duration, output_duration),
        };
        state.record(CheckId::Duration, length)?;

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

        // Plays start to finish. The decoded length is compared with what
        // the output claims for its picture and sound, so a file cut short
        // on disk fails even though its header looks fine.
        let thorough = req.level == ValidationLevel::Thorough;
        let expected_len = expected_play_length(&output, source_duration);
        let (decode_weight, recheck_weight) = split_weight(self.weights.decode, 0.85);
        let decode = self
            .decode_output(&output, expected_len, decode_weight)
            .await?;
        let inherited = match source.as_ref() {
            Some(src) if !decode.damage.is_empty() => {
                self.inherited_damage(&decode.damage, &output, src, recheck_weight)
                    .await?
            }
            _ => Inherited::default(),
        };
        self.progress(self.weights.decode, 1.0);
        state.record(
            CheckId::Decode,
            decode_check(&decode, expected_len, &inherited),
        )?;

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
        let Some(align) = Alignment::new(req.source_probe, source.as_ref(), &output) else {
            return state.record(
                CheckId::Visual,
                CheckResult::fail("The video track could not be found for comparison", None),
            );
        };
        let mut visual = self.visual(&align, video_len, segments).await?;
        state.ssim_min = visual.ssim_min;
        state.ssim_avg = visual.ssim_avg;
        state.psnr_avg = visual.psnr_avg;
        if !thorough || visual.result.status == CheckStatus::Fail {
            return state.record(CheckId::Visual, visual.result);
        }

        // Thorough: every frame at low resolution, plus black and frozen
        // video measured the same way on both files.
        self.check_cancel()?;
        let found = common_alignment(&visual.alignments).unwrap_or(SegmentAlignment {
            shift: align.av_timeline_shift,
            frames: 0,
        });
        let scan = self.scan(&align, found, video_len).await?;
        let (black, frozen) = match &scan {
            Some(scan) => {
                visual.result = merge_scan(visual.result, scan, &align);
                (
                    added_time_check(&scan.source.black, &scan.output.black, "black"),
                    added_time_check(&scan.source.frozen, &scan.output.frozen, "frozen"),
                )
            }
            None => (
                CheckResult::skipped("The black-frame scan could not run"),
                CheckResult::skipped("The frozen-frame scan could not run"),
            ),
        };
        state.record(CheckId::Visual, visual.result)?;
        state.record(CheckId::BlackFrames, black)?;
        state.record(CheckId::FrozenFrames, frozen)?;
        self.progress(self.weights.scan, 1.0);
        Ok(())
    }

    /// Full decode of the output's picture and sound, collecting damage.
    async fn decode_output(
        &self,
        output: &MediaProbe,
        expected_len: Option<f64>,
        weight: (f32, f32),
    ) -> Result<DecodeResult, Stop> {
        let req = self.req;
        let video_map = output
            .primary_video()
            .map_or_else(|| "0:v:0".to_string(), |v| format!("0:{}", v.index));
        let maps = [video_map, "0:a?".to_string()];
        let duration = output.best_duration().or(expected_len);
        let (exit, damage, decoded_secs) = self
            .decode_streams(req.output, &maps, duration, weight)
            .await?;
        Ok(DecodeResult {
            exit,
            damage,
            decoded_secs,
        })
    }

    /// Decode `maps` of `path` to nowhere at `level+info`, tallying damage.
    async fn decode_streams(
        &self,
        path: &Path,
        maps: &[String],
        duration: Option<f64>,
        weight: (f32, f32),
    ) -> Result<(FfmpegExit, DamageLog, Option<f64>), Stop> {
        let mut args: Vec<String> = ["-nostdin", "-hide_banner", "-loglevel", "level+info", "-i"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        args.push(path.to_string_lossy().into_owned());
        for map in maps {
            args.extend(["-map".to_string(), map.clone()]);
        }
        args.extend(
            ["-progress", "pipe:1", "-nostats", "-f", "null", "-"]
                .iter()
                .map(|s| s.to_string()),
        );

        let mut damage = DamageLog::default();
        let mut decoded_secs: Option<f64> = None;
        let started = Instant::now();
        let exit = run_ffmpeg(
            &self.command(&args, DECODE_STALL),
            self.cancel,
            &mut |block: &ProgressBlock| {
                if let Some(t) = block.out_time_secs {
                    decoded_secs = Some(decoded_secs.map_or(t, |d: f64| d.max(t)));
                }
                let p = compute_progress(block, duration, started.elapsed().as_secs_f64());
                self.progress(weight, p.percent / 100.0);
            },
            &mut |line: &str| damage.push(line),
        )
        .await;
        if exit == FfmpegExit::Cancelled {
            return Err(Stop::Cancelled);
        }
        Ok((exit, damage, decoded_secs))
    }

    /// Which of the output's damaged tracks carry damage the original
    /// already had. Tracks whose codec also appears in the original (copied
    /// tracks) are decoded from the original and their damage counted.
    async fn inherited_damage(
        &self,
        damage: &DamageLog,
        output: &MediaProbe,
        source: &MediaProbe,
        weight: (f32, f32),
    ) -> Result<Inherited, Stop> {
        let candidates: Vec<&str> = damage
            .by_decoder
            .keys()
            .map(String::as_str)
            .filter(|key| output.has_codec(key) && source.has_codec(key))
            .collect();
        if candidates.is_empty() {
            return Ok(Inherited::default());
        }
        let maps: Vec<String> = source
            .streams
            .iter()
            .filter(|s| {
                !s.attached_pic
                    && matches!(s.kind, Some(StreamKind::Video | StreamKind::Audio))
                    && candidates.contains(&s.codec.as_str())
            })
            .map(|s| format!("0:{}", s.index))
            .collect();
        let (_, source_damage, _) = self
            .decode_streams(self.req.source, &maps, source.best_duration(), weight)
            .await?;
        let mut inherited = Inherited::default();
        for key in candidates {
            let out = damage.count(key);
            let src = source_damage.count(key);
            if src > 0 && out <= src + (src / 10).max(2) {
                let kind = output.kind_of_codec(key).unwrap_or("track");
                inherited.decoders.insert(key.to_string(), (kind, src));
            }
        }
        Ok(inherited)
    }

    /// Compare pictures at `count` points spread over the file.
    async fn visual(
        &self,
        align: &Alignment,
        source_duration: Option<f64>,
        count: usize,
    ) -> Result<VisualOutcome, Stop> {
        let Some(duration) = source_duration.filter(|d| *d >= 1.0) else {
            return Ok(VisualOutcome::only(CheckResult::skipped(
                "The file is too short to compare pictures",
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
                    "The pictures weren't compared because the scratch files for it couldn't be \
                     made: {}",
                    szalinski_core::plain::io_reason(&e)
                ))));
            }
        };
        let mut measured = Vec::with_capacity(segments.len());
        let mut failure = None;
        for (i, &(start, len)) in segments.iter().enumerate() {
            self.progress(self.weights.visual, i as f32 / segments.len() as f32);
            match self.compare_segment(align, dir.path(), i, start, len).await {
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

    /// Compare one segment at the best alignment. The expected alignment
    /// (the shared timeline) is searched first; if that matches poorly and
    /// aligning on each file's first frame is a different guess, that is
    /// searched too, and the better match wins.
    async fn compare_segment(
        &self,
        align: &Alignment,
        dir: &Path,
        index: usize,
        start: f64,
        len: f64,
    ) -> Result<SegmentStats, SegmentError> {
        let mut best: Option<SegmentStats> = None;
        for (n, shift) in align.shift_candidates().into_iter().enumerate() {
            let seg = self
                .compare_segment_at(align, dir, index * 10 + n, start, len, shift)
                .await?;
            let good = seg.mean() >= OVERALL_SSIM_WARN;
            if best.as_ref().is_none_or(|b| seg.mean() > b.mean()) {
                best = Some(seg);
            }
            if good {
                break;
            }
        }
        best.ok_or_else(|| {
            SegmentError::Failed(format!(
                "No picture could be read from the new file at {}",
                format_time(start)
            ))
        })
    }

    /// Compare one segment around one alignment guess. If seeking in the
    /// source overshot the segment (no frames compared), the segment is
    /// compared once more reading the source from its start.
    async fn compare_segment_at(
        &self,
        align: &Alignment,
        dir: &Path,
        index: usize,
        start: f64,
        len: f64,
        shift: f64,
    ) -> Result<SegmentStats, SegmentError> {
        let at = align.seek_point(start);
        let before = align.search_before(at);
        for preroll in [Some(SEEK_PREROLL_SECS), None] {
            let window = SegmentWindow {
                at,
                len,
                shift,
                search_before: before,
                search_after: ALIGN_SEARCH_FRAMES,
                source_preroll: preroll,
            };
            let paths = stats_paths(dir, index, window.offsets());
            self.run_segment(align, &window, &paths, start).await?;
            let mut best: Option<SegmentStats> = None;
            for (j, ssim_path, psnr_path) in &paths {
                let ssim = tokio::fs::read_to_string(ssim_path)
                    .await
                    .map(|t| parse_ssim_stats(&t))
                    .unwrap_or_default();
                let mse = tokio::fs::read_to_string(psnr_path)
                    .await
                    .map(|t| parse_psnr_stats(&t))
                    .unwrap_or_default();
                if ssim.is_empty() {
                    continue;
                }
                let seg = SegmentStats {
                    start,
                    ssim,
                    mse,
                    alignment: SegmentAlignment { shift, frames: *j },
                };
                // Prefer the smallest offset among equally good matches (a
                // still picture matches everywhere).
                let better = best.as_ref().is_none_or(|b| {
                    seg.mean() > b.mean() + 1e-4
                        || ((seg.mean() - b.mean()).abs() <= 1e-4
                            && seg.alignment.frames.abs() < b.alignment.frames.abs())
                });
                if better {
                    best = Some(seg);
                }
            }
            if let Some(seg) = best {
                return Ok(seg);
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
        paths: &[(i64, PathBuf, PathBuf)],
        start: f64,
    ) -> Result<(), SegmentError> {
        let args = segment_args(self.req, align, window, paths);
        match run_ffmpeg(
            &self.command(&args, SEGMENT_STALL),
            self.cancel,
            &mut |_| {},
            &mut |_| {},
        )
        .await
        {
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

    /// Thorough level: decode both files completely at low resolution,
    /// compare every frame and measure black and frozen video on both.
    /// `None` when the scan could not run (the checks are then skipped).
    async fn scan(
        &self,
        align: &Alignment,
        found: SegmentAlignment,
        source_duration: Option<f64>,
    ) -> Result<Option<ScanOutcome>, Stop> {
        let dir = match make_stats_dir().await {
            Ok(dir) => dir,
            Err(e) => {
                tracing::debug!("no scratch space for the full-length scan: {e}");
                return Ok(None);
            }
        };
        let paths: Vec<PathBuf> = (-SCAN_SEARCH_FRAMES..=SCAN_SEARCH_FRAMES)
            .map(|j| {
                dir.path()
                    .join(format!("scan{}.log", j + SCAN_SEARCH_FRAMES))
            })
            .collect();
        let plan = ScanPlan::new(align, found);
        let args = scan_args(self.req, align, &plan, &paths);
        let mut source_log = DetectLog::default();
        let mut output_log = DetectLog::default();
        let started = Instant::now();
        let weight = self.weights.scan;
        let exit = run_ffmpeg(
            &self.command(&args, DECODE_STALL),
            self.cancel,
            &mut |block: &ProgressBlock| {
                let p = compute_progress(block, source_duration, started.elapsed().as_secs_f64());
                self.progress(weight, p.percent / 100.0);
            },
            &mut |line: &str| {
                let (context, _, message) = split_log_line(line);
                match context {
                    Some(c) if c.ends_with("@src") => source_log.push(message),
                    Some(c) if c.ends_with("@out") => output_log.push(message),
                    _ => {}
                }
            },
        )
        .await;
        let result = match exit {
            FfmpegExit::Cancelled => Err(Stop::Cancelled),
            FfmpegExit::Success { .. } => {
                let mut per_offset = Vec::with_capacity(paths.len());
                for path in &paths {
                    per_offset.push(read_ssim_file(path).await.unwrap_or_default());
                }
                let frames = best_per_frame(&per_offset);
                // Timestamps inside the scan start at 0 on both sides; close
                // intervals still open at the end of the compared video.
                let end = frames.len() as f64 / align.rate + 1.0 / align.rate;
                Ok((!frames.is_empty()).then(|| ScanOutcome {
                    frames,
                    rate: align.rate,
                    start_offset: plan.start_offset,
                    source: source_log.finish(end),
                    output: output_log.finish(end),
                }))
            }
            other => {
                tracing::debug!("full-length scan failed: {other:?}");
                Ok(None)
            }
        };
        discard_dir(dir).await;
        result
    }
}

/// Median of `values` (`None` when empty).
fn median(values: &[f64]) -> Option<f64> {
    let mut sorted: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    Some(sorted[sorted.len() / 2])
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
            format!(
                "The new file's video is {found} instead of {}",
                target.label()
            ),
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

/// "1 video track, 2 audio tracks and 1 subtitle track".
fn describe_counts(c: StreamSummary) -> String {
    let count =
        |n: u32, kind: &str| format!("{n} {kind} {}", if n == 1 { "track" } else { "tracks" });
    let mut parts = vec![count(c.video, "video")];
    if c.audio > 0 || c.subtitle > 0 {
        parts.push(count(c.audio, "audio"));
    }
    if c.subtitle > 0 {
        parts.push(count(c.subtitle, "subtitle"));
    }
    match parts.len() {
        1 => parts.remove(0),
        _ => {
            let last = parts.pop().unwrap_or_default();
            format!("{} and {last}", parts.join(", "))
        }
    }
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

/// How far a full decode of the new file must get: the length it claims
/// for its picture and sound, else its container's, else the original's.
fn expected_play_length(output: &MediaProbe, source_duration: Option<f64>) -> Option<f64> {
    output
        .av_duration()
        .or(output.best_duration())
        .or(source_duration)
}

/// Allowed duration difference: max(1 s, 0.5 %).
pub(crate) fn duration_tolerance(source_secs: f64) -> f64 {
    (source_secs * 0.005).max(1.0)
}

/// How a failed length check begins when the original's length couldn't
/// be read (the job's advice differs then; see `run::verification_error`).
pub(crate) const UNREAD_SOURCE_LENGTH: &str = "The original's length couldn't be read";

/// The length check's detail for an original whose length is unknown (see
/// [`MediaProbe::length_unknown`]): `cut_off` when its packets were listed
/// and it stops in the middle of one.
fn unread_source_length(cut_off: bool) -> String {
    let why = if cut_off {
        "it stops in the middle of its data and doesn't say how long it should be"
    } else {
        "it states none that can be trusted, and its contents couldn't be listed"
    };
    format!("{UNREAD_SOURCE_LENGTH} ({why}), so the new file couldn't be checked against it")
}

/// Compare the two lengths. When the original's is unknown the check
/// fails: skipping it would pass a new file cut short as readily as a
/// complete one.
fn duration_check(source: Option<f64>, output: Option<f64>) -> CheckResult {
    let Some(src) = source else {
        return CheckResult::fail(
            format!("{UNREAD_SOURCE_LENGTH}, so the new file couldn't be checked against it"),
            None,
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
        let compared = if out < src { "shorter" } else { "longer" };
        CheckResult::fail(
            format!(
                "The new file is {compared} than the original ({} instead of {})",
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
    damage: DamageLog,
    decoded_secs: Option<f64>,
}

/// Damaged tracks of the output whose damage the original already has:
/// decoder name → (track kind, damage lines in the original).
#[derive(Debug, Default)]
struct Inherited {
    decoders: BTreeMap<String, (&'static str, usize)>,
}

fn decode_check(
    decode: &DecodeResult,
    expected_len: Option<f64>,
    inherited: &Inherited,
) -> CheckResult {
    let new_damage: Vec<(&String, &DecoderDamage)> = decode
        .damage
        .by_decoder
        .iter()
        .filter(|(key, _)| !inherited.decoders.contains_key(*key))
        .collect();
    if let Some((_, first)) = new_damage.first() {
        let total: usize = new_damage.iter().map(|(_, d)| d.count).sum();
        let more = match total {
            0 | 1 => String::new(),
            n => format!(" ({} more)", n - 1),
        };
        return CheckResult::fail(
            format!("Found a playback error: {}{more}", first.first),
            None,
        );
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
    if let (Some(expected), Some(got)) = (expected_len, decoded)
        && got + duration_tolerance(expected) < expected
    {
        return CheckResult::fail(
            format!(
                "Playback stopped at {} of {}",
                format_time(got),
                format_time(expected)
            ),
            Some(got),
        );
    }
    if !inherited.decoders.is_empty() {
        let tracks: Vec<String> = inherited
            .decoders
            .iter()
            .map(|(codec, (kind, _))| format!("{kind} ({codec})"))
            .collect();
        return CheckResult::warn(
            format!(
                "The original's {} already had playback errors; the new file has the same ones and no new ones",
                tracks.join(" and ")
            ),
            decoded,
        );
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

/// Compare black or frozen intervals: the new file may add at most
/// [`MAX_ADDED_SECS`] that the original does not have at the same place.
fn added_time_check(source: &[(f64, f64)], output: &[(f64, f64)], what: &str) -> CheckResult {
    // `Iterator::sum` of nothing is -0.0 for floats; fold from +0.0.
    let total = |list: &[(f64, f64)]| list.iter().fold(0.0, |acc, (a, b)| acc + (b - a));
    let widened: Vec<(f64, f64)> = source
        .iter()
        .map(|(a, b)| (a - INTERVAL_SLACK_SECS, b + INTERVAL_SLACK_SECS))
        .collect();
    let added = subtract_intervals(output, &widened);
    let added_secs = total(&added);
    if added_secs > MAX_ADDED_SECS {
        let at = added
            .iter()
            .max_by(|x, y| (x.1 - x.0).total_cmp(&(y.1 - y.0)))
            .map(|(a, _)| format!(", starting near {}", format_time(*a)))
            .unwrap_or_default();
        return CheckResult::fail(
            format!("The new file has {added_secs:.1} s more {what} video than the original{at}"),
            Some(added_secs),
        );
    }
    CheckResult::pass(
        format!(
            "{} video: {:.1} s in the original, {:.1} s in the new file",
            capitalize(what),
            total(source),
            total(output)
        ),
        Some(added_secs),
    )
}

/// The parts of `from` not covered by any interval in `remove`.
fn subtract_intervals(from: &[(f64, f64)], remove: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut remove: Vec<(f64, f64)> = remove.to_vec();
    remove.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out = Vec::new();
    for &(start, end) in from {
        let mut cursor = start;
        for &(r0, r1) in &remove {
            if r1 <= cursor || r0 >= end {
                continue;
            }
            if r0 > cursor {
                out.push((cursor, r0));
            }
            cursor = cursor.max(r1);
            if cursor >= end {
                break;
            }
        }
        if cursor < end {
            out.push((cursor, end));
        }
    }
    out.retain(|(a, b)| b > a);
    out
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Damage reported by one decoder (or the demuxer) during a decode pass.
#[derive(Debug, Clone, PartialEq)]
struct DecoderDamage {
    count: usize,
    /// The first message, with the decoder in brackets.
    first: String,
}

/// Damage lines of a decode pass run at `-loglevel level+info`, by decoder.
///
/// Damage is any error-level line, the "corrupt decoded frame" warning
/// ffmpeg prints for every frame a decoder had to patch up, and the
/// error-concealment notices (`concealing N DC, N AC, N MV errors`). The
/// last two appear without any error when a decoder hides corruption.
#[derive(Debug, Default, Clone)]
struct DamageLog {
    by_decoder: BTreeMap<String, DecoderDamage>,
}

impl DamageLog {
    fn push(&mut self, line: &str) {
        let (context, level, message) = split_log_line(line);
        let message = message.trim();
        let damaged = match level {
            Some("error" | "fatal" | "panic") => true,
            Some("warning") => message.contains("corrupt decoded frame"),
            Some("info") => message.starts_with("concealing "),
            _ => false,
        };
        if !damaged || message.is_empty() {
            return;
        }
        let key = decoder_key(context);
        let entry = self
            .by_decoder
            .entry(key.clone())
            .or_insert_with(|| DecoderDamage {
                count: 0,
                first: if key.is_empty() {
                    message.to_string()
                } else {
                    format!("{message} ({key})")
                },
            });
        entry.count += 1;
    }

    fn is_empty(&self) -> bool {
        self.by_decoder.is_empty()
    }

    fn count(&self, key: &str) -> usize {
        self.by_decoder.get(key).map_or(0, |d| d.count)
    }
}

/// The decoder (codec name) or demuxer a log context belongs to:
/// `vist#0:0/h264` and `h264` → `h264`, `in#0/matroska,webm` →
/// `matroska,webm`.
fn decoder_key(context: Option<&str>) -> String {
    let Some(context) = context else {
        return String::new();
    };
    match context.split_once('/') {
        Some((_, name)) => name.to_string(),
        None => context.to_string(),
    }
}

/// Black and frozen intervals from `blackdetect`/`freezedetect` log lines
/// of one side of the full-length scan (seconds from the scan's start).
#[derive(Debug, Clone, Default, PartialEq)]
struct DetectLog {
    black: Vec<(f64, f64)>,
    frozen: Vec<(f64, f64)>,
    /// A freeze that started but has not ended yet.
    open_freeze: Option<f64>,
}

impl DetectLog {
    /// Feed one log message (prefixes already removed).
    fn push(&mut self, message: &str) {
        if let (Some(start), Some(end)) = (
            value_after(message, "black_start:"),
            value_after(message, "black_end:"),
        ) {
            if end > start {
                self.black.push((start, end));
            }
        } else if let Some(start) = value_after(message, "lavfi.freezedetect.freeze_start:") {
            self.open_freeze = Some(start);
        } else if let Some(end) = value_after(message, "lavfi.freezedetect.freeze_end:")
            && let Some(start) = self.open_freeze.take()
            && end > start
        {
            self.frozen.push((start, end));
        }
    }

    /// Close a freeze still running at `end`.
    fn finish(mut self, end: f64) -> Self {
        if let Some(start) = self.open_freeze.take()
            && end > start
        {
            self.frozen.push((start, end));
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

/// Per-frame SSIM "All" values from a (possibly large) stats file, read
/// line by line.
async fn read_ssim_file(path: &Path) -> std::io::Result<Vec<f64>> {
    let file = tokio::fs::File::open(path).await?;
    let mut lines = tokio::io::BufReader::new(file).lines();
    let mut values = Vec::new();
    while let Some(line) = lines.next_line().await? {
        if let Some(v) = value_after(&line, "All:") {
            values.push(v);
        }
    }
    Ok(values)
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
    /// The best match: the alignment guess that was searched (seconds, see
    /// [`Alignment::timeline_shift`]) and the offset in frames from it.
    alignment: SegmentAlignment,
}

/// Where a segment matched: output window trimmed at `at + shift`, each of
/// its frames paired with the reference frame `frames` later.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SegmentAlignment {
    shift: f64,
    frames: i64,
}

/// The alignment most segments agree on: the most common guess, and the
/// median frame offset of the segments that used it.
fn common_alignment(found: &[SegmentAlignment]) -> Option<SegmentAlignment> {
    let mut best: Option<(f64, usize)> = None;
    for a in found {
        let count = found.iter().filter(|b| b.shift == a.shift).count();
        if best.is_none_or(|(_, c)| count > c) {
            best = Some((a.shift, count));
        }
    }
    let (shift, _) = best?;
    let mut frames: Vec<i64> = found
        .iter()
        .filter(|a| a.shift == shift)
        .map(|a| a.frames)
        .collect();
    frames.sort_unstable();
    Some(SegmentAlignment {
        shift,
        frames: frames[frames.len() / 2],
    })
}

impl SegmentStats {
    fn mean(&self) -> f64 {
        if self.ssim.is_empty() {
            return 0.0;
        }
        self.ssim.iter().sum::<f64>() / self.ssim.len() as f64
    }
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
    /// Alignment found in each measured segment.
    alignments: Vec<SegmentAlignment>,
}

impl VisualOutcome {
    fn only(result: CheckResult) -> Self {
        Self {
            result,
            ssim_min: None,
            ssim_avg: None,
            psnr_avg: None,
            alignments: Vec::new(),
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
        .map(|s| (s.start, s.mean()))
        .min_by(|a, b| a.1.total_cmp(&b.1));

    let result = match (worst_frame, worst_segment) {
        (Some((at, v)), _) if v < FRAME_SSIM_FAIL => CheckResult::fail(
            format!(
                "A frame near {} looks very different from the original ({} similar)",
                format_time(at),
                similarity(v)
            ),
            Some(ssim_avg),
        ),
        (_, Some((at, v))) if v < SEGMENT_SSIM_FAIL => CheckResult::fail(
            format!(
                "The picture near {} doesn't match the original ({} similar)",
                format_time(at),
                similarity(v)
            ),
            Some(ssim_avg),
        ),
        _ if ssim_avg < OVERALL_SSIM_WARN => CheckResult::warn(
            format!(
                "Noticeably softer than the original ({} similar on average at {} points)",
                similarity(ssim_avg),
                segments.len()
            ),
            Some(ssim_avg),
        ),
        _ => CheckResult::pass(
            format!(
                "Matches the original at {} points ({} similar on average)",
                segments.len(),
                similarity(ssim_avg)
            ),
            Some(ssim_avg),
        ),
    };
    VisualOutcome {
        result,
        ssim_min: Some(ssim_min),
        ssim_avg: Some(ssim_avg),
        psnr_avg,
        alignments: segments.iter().map(|s| s.alignment).collect(),
    }
}

/// Result of the full-length scan (thorough level).
#[derive(Debug, Clone, PartialEq)]
struct ScanOutcome {
    /// Best SSIM per compared frame (low resolution).
    frames: Vec<f64>,
    /// Frame rate of the comparison grid.
    rate: f64,
    /// Where the scan starts, in seconds from the original's video start.
    start_offset: f64,
    source: DetectLog,
    output: DetectLog,
}

/// For each frame, the best SSIM across the searched offsets.
fn best_per_frame(per_offset: &[Vec<f64>]) -> Vec<f64> {
    let len = per_offset.iter().map(Vec::len).max().unwrap_or(0);
    (0..len)
        .map(|i| {
            per_offset
                .iter()
                .filter_map(|v| v.get(i).copied())
                .fold(f64::NEG_INFINITY, f64::max)
        })
        .collect()
}

/// A stretch of frames that does not match: `(first frame, frame count,
/// lowest similarity)`.
type BadRun = (usize, usize, f64);

/// Judge the full-length scan. `Some(fail)` when a stretch of frames does
/// not match the original:
/// - [`SCAN_MIN_BAD_FRAMES`] or more frames in a row below
///   [`FRAME_SSIM_FAIL`] (garbage frames),
/// - any second whose mean is below [`SEGMENT_SSIM_FAIL`], or
/// - [`SCAN_ANOMALY_SECS`] or more where every frame is at least
///   [`SCAN_ANOMALY_DROP`] below the file's typical similarity and below
///   [`OVERALL_SSIM_WARN`] (a burst of green blocks or smearing in an
///   otherwise clean file).
fn judge_scan(scan: &ScanOutcome) -> Option<(BadRun, &'static str)> {
    let frames = &scan.frames;
    if frames.is_empty() {
        return None;
    }
    let rate = if scan.rate.is_finite() && scan.rate > 0.0 {
        scan.rate
    } else {
        25.0
    };
    if let Some(run) = longest_run(frames, |v| v < FRAME_SSIM_FAIL)
        && run.1 >= SCAN_MIN_BAD_FRAMES
    {
        return Some((run, "garbage"));
    }
    let window = (rate.round() as usize).clamp(1, frames.len());
    let mut sum: f64 = frames[..window].iter().sum();
    let mut worst = (0usize, sum / window as f64);
    for i in window..frames.len() {
        sum += frames[i] - frames[i - window];
        let mean = sum / window as f64;
        if mean < worst.1 {
            worst = (i + 1 - window, mean);
        }
    }
    if worst.1 < SEGMENT_SSIM_FAIL {
        let low = frames[worst.0..worst.0 + window]
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min);
        return Some(((worst.0, window, low.min(worst.1)), "mismatch"));
    }
    let typical = median(frames)?;
    let limit = (typical - SCAN_ANOMALY_DROP).min(OVERALL_SSIM_WARN);
    let min_frames = ((SCAN_ANOMALY_SECS * rate).round() as usize).max(SCAN_MIN_BAD_FRAMES);
    match longest_run(frames, |v| v < limit) {
        Some(run) if run.1 >= min_frames => Some((run, "burst")),
        _ => None,
    }
}

/// The longest run of consecutive values matching `bad`.
fn longest_run(values: &[f64], bad: impl Fn(f64) -> bool) -> Option<BadRun> {
    let mut best: Option<BadRun> = None;
    let mut current: Option<BadRun> = None;
    for (i, &v) in values.iter().enumerate() {
        if bad(v) {
            let run = current.get_or_insert((i, 0, f64::INFINITY));
            run.1 += 1;
            run.2 = run.2.min(v);
            if best.is_none_or(|b| run.1 > b.1) {
                best = Some(*run);
            }
        } else {
            current = None;
        }
    }
    best
}

/// A similarity score (SSIM, 0 to 1) as the percentage the UI shows:
/// `0.998` is "99.8%", and anything that rounds to 100 is "100%".
fn similarity(ssim: f64) -> String {
    let pct = ssim * 100.0;
    if pct >= 99.95 {
        "100%".to_string()
    } else {
        format!("{pct:.1}%")
    }
}

/// Fold the full-length scan into the segment result.
fn merge_scan(segments: CheckResult, scan: &ScanOutcome, align: &Alignment) -> CheckResult {
    let rate = if align.rate > 0.0 { align.rate } else { 25.0 };
    match judge_scan(scan) {
        Some(((first, count, low), kind)) => {
            let from = scan.start_offset + first as f64 / rate;
            let to = from + count as f64 / rate;
            let what = match kind {
                "garbage" => "is damaged",
                "burst" => "suddenly looks different",
                _ => "doesn't match the original",
            };
            CheckResult::fail(
                format!(
                    "The picture from {} to {} {what} ({} similar)",
                    format_time(from),
                    format_time(to),
                    similarity(low)
                ),
                segments.value,
            )
        }
        None if segments.status == CheckStatus::Pass => CheckResult::pass(
            format!(
                "{}, and all {} frames match at low resolution",
                segments.detail,
                scan.frames.len()
            ),
            segments.value,
        ),
        None => segments,
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
// Comparison commands

/// How to line up source and output for a picture comparison.
///
/// Segment comparisons run with `-copyts`, so frame timestamps inside the
/// filter graph are each file's own timestamps. Each side is seeked a
/// little early and then cut with `trim`, which is exact even when seeking
/// is not (MPEG-TS, AVI without index).
///
/// Where the matching picture is: ffmpeg moves every timestamp of the
/// source by the source's container start, so a picture at `t` seconds
/// after the source's first video frame is at about
/// `output_video_start + t + timeline_shift` in the output. The shift is
/// non-zero when the output's video was padded at the start (MP4 keeps a
/// constant frame rate and fills the gap to the audio with repeated frames)
/// or when the source's video starts after its audio.
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
    /// See the type's documentation.
    timeline_shift: f64,
    /// The same, measured from the earliest picture or sound track rather
    /// than the container start (ffmpeg's shift ignores tracks it does not
    /// use, such as subtitles that start early).
    av_timeline_shift: f64,
    /// Output picture size; the source is scaled to it.
    width: u32,
    height: u32,
    /// Whether the source's picture size differs from the output's.
    scale_source: bool,
    /// Size both sides are reduced to for the segment comparison.
    compare_size: (u32, u32),
    /// Size both sides are reduced to for the full-length scan.
    scan_size: (u32, u32),
    /// Frame grid both sides are put on (`fps=` expression) and its rate.
    rate_expr: String,
    rate: f64,
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
        let info_video = source_info.primary_video();
        let source_index = src_video
            .map(|v| v.index)
            .or_else(|| info_video.map(|v| v.index))?;
        let source_size = src_video
            .and_then(|v| Some((v.width?, v.height?)))
            .or_else(|| info_video.and_then(|v| Some((v.width?, v.height?))));

        let known_format_start = source.and_then(|s| s.start_time).or(source_info.start_time);
        let source_format_start = known_format_start.unwrap_or(0.0);
        let source_video_start = src_video
            .and_then(|v| v.start_time)
            .unwrap_or(source_format_start);
        let output_format_start = output.start_time.unwrap_or(0.0);
        let output_video_start = out_video.start_time.unwrap_or(output_format_start);
        let timeline_shift = match known_format_start {
            Some(start) => (source_video_start - start) - output_video_start,
            None => 0.0,
        };
        let av_start = source.and_then(|s| {
            s.streams
                .iter()
                .filter(|st| {
                    !st.attached_pic
                        && matches!(st.kind, Some(StreamKind::Video | StreamKind::Audio))
                })
                .filter_map(|st| st.start_time)
                .reduce(f64::min)
        });
        let av_timeline_shift = match av_start {
            Some(start) => (source_video_start - start) - output_video_start,
            None => timeline_shift,
        };

        let source_fps = src_video
            .and_then(|v| v.frame_rate.as_ref().map(Rational::value))
            .or_else(|| info_video.and_then(|v| v.frame_rate))
            .filter(|f| sane_fps(*f));
        let out_rate = out_video.frame_rate.filter(|r| sane_fps(r.value()));
        let rate_expr = out_rate
            .as_ref()
            .map(Rational::expr)
            .or_else(|| source_fps.map(|f| format!("{f:.6}")))
            .unwrap_or_else(|| "25".to_string());
        let out_fps = out_rate.as_ref().map(Rational::value);
        let rate = out_fps.or(source_fps).unwrap_or(25.0);

        let source_interlaced =
            src_video.is_some_and(|v| v.interlaced) || info_video.is_some_and(|v| v.interlaced);
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
            timeline_shift,
            av_timeline_shift,
            width,
            height,
            scale_source: source_size != Some((width, height)),
            compare_size: reduced_size(width, height, COMPARE_PIXELS),
            scan_size: reduced_size(width, height, SCAN_PIXELS),
            rate_expr,
            rate,
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

    /// Frames available before `at` for the alignment search.
    fn search_before(&self, at: f64) -> i64 {
        ((at * self.rate).floor() as i64).clamp(0, ALIGN_SEARCH_FRAMES)
    }

    /// Alignment guesses to search, best first: the shared timeline of the
    /// picture and sound tracks, of the whole container, then each file's
    /// first frame. Guesses within one search window of an earlier one are
    /// left out.
    fn shift_candidates(&self) -> Vec<f64> {
        let reach = ALIGN_SEARCH_FRAMES as f64 / self.rate;
        let mut shifts: Vec<f64> = Vec::with_capacity(3);
        for shift in [self.av_timeline_shift, self.timeline_shift, 0.0] {
            if shift.is_finite() && shifts.iter().all(|s| (s - shift).abs() > reach) {
                shifts.push(shift);
            }
        }
        shifts
    }
}

/// `width`x`height` scaled down (never up) to about `pixels`, keeping the
/// aspect ratio, with even sides of at least 16.
fn reduced_size(width: u32, height: u32, pixels: f64) -> (u32, u32) {
    let area = f64::from(width) * f64::from(height);
    let factor = if area > pixels {
        (pixels / area).sqrt()
    } else {
        1.0
    };
    let even = |v: u32| ((f64::from(v) * factor / 2.0).round() as u32 * 2).max(16);
    (even(width), even(height))
}

fn sane_fps(f: f64) -> bool {
    f.is_finite() && (1.0..=300.0).contains(&f)
}

/// One comparison window: `at..at + len` seconds after the source's video
/// start, and the matching window of the output, `shift` seconds later on
/// its timeline, searched from `search_before` frames earlier to
/// `search_after` frames later.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SegmentWindow {
    at: f64,
    len: f64,
    shift: f64,
    search_before: i64,
    search_after: i64,
    /// Seek this many seconds early in the source (the output, which we
    /// wrote with an index, gets at most a second). `None` reads both files
    /// from the start.
    source_preroll: Option<f64>,
}

impl SegmentWindow {
    /// The offsets searched, in frames: output frame `i` is compared with
    /// the reference frame `offset` frames after the one at `at + i`.
    fn offsets(&self) -> std::ops::RangeInclusive<i64> {
        -self.search_before..=self.search_after
    }
}

/// Input options that read `from..to` seconds after a file's video start:
/// `-ss`/`-t` relative to the container start, reading from `preroll`
/// seconds early.
fn input_window(
    video_start: f64,
    format_start: f64,
    from: f64,
    to: f64,
    preroll: Option<f64>,
) -> Vec<String> {
    let offset = video_start - format_start;
    let end = offset + to;
    let seek = preroll.map_or(0.0, |p| (offset + from - p).max(0.0));
    let mut args = Vec::with_capacity(4);
    if seek > 0.0 {
        args.extend(["-ss".to_string(), format!("{seek:.6}")]);
    }
    args.extend(["-t".to_string(), format!("{:.6}", end - seek + 1.0)]);
    args
}

/// Filters that bring the source's primary video onto the output's frame
/// grid and size, starting at `trim_start` (a timestamp).
fn source_chain(align: &Alignment, trim: &str) -> String {
    let deinterlace = align
        .deinterlace
        .map(|mode| format!("bwdif=mode={mode},"))
        .unwrap_or_default();
    let scale = if align.scale_source {
        format!(",scale={}:{}:flags=bicubic", align.width, align.height)
    } else {
        String::new()
    };
    format!(
        "[0:{si}]{deinterlace}{trim},setpts=PTS-STARTPTS,fps={rate}{scale}",
        si = align.source_index,
        rate = align.rate_expr,
    )
}

/// The ffmpeg command for one segment: the output window is compared with
/// the reference at every offset in `paths` (offset in frames, SSIM stats
/// file, PSNR stats file), all at [`Alignment::compare_size`].
///
/// The reference starts `search_before` frames before the output window
/// and runs `search_after` frames past it; output frame `i` is compared
/// with reference frame `i + search_before + offset`.
fn segment_args(
    req: &ValidateRequest<'_>,
    align: &Alignment,
    window: &SegmentWindow,
    paths: &[(i64, PathBuf, PathBuf)],
) -> Vec<String> {
    let frame = 1.0 / align.rate;
    let ref_from = window.at - window.search_before as f64 * frame;
    let ref_to = window.at + window.len + window.search_after as f64 * frame;
    let out_from = window.at + window.shift;
    let out_to = out_from + window.len;
    let (cw, ch) = align.compare_size;
    let n = paths.len();
    let src_trim = format!(
        "trim=start={:.6}:end={:.6}",
        align.source_video_start + ref_from,
        align.source_video_start + ref_to + frame / 2.0
    );
    let out_trim = format!(
        "trim=start={:.6}:end={:.6}",
        align.output_video_start + out_from,
        align.output_video_start + out_to
    );
    let labels = |prefix: &str| (0..n).map(|k| format!("[{prefix}{k}]")).collect::<String>();
    let mut graph = format!(
        "{},scale={cw}:{ch}:flags=area,format=yuv420p,split={n}{};\
         [1:{oi}]{out_trim},setpts=PTS-STARTPTS,fps={rate},scale={cw}:{ch}:flags=area,\
         format=yuv420p,split={n}{}",
        source_chain(align, &src_trim),
        labels("r"),
        labels("o"),
        oi = align.output_index,
        rate = align.rate_expr,
    );
    for (k, (offset, ssim, psnr)) in paths.iter().enumerate() {
        let skip = window.search_before + offset;
        graph.push_str(&format!(
            ";[r{k}]trim=start_frame={skip},setpts=PTS-STARTPTS,split=2[ra{k}][rb{k}];\
             [o{k}]split=2[oa{k}][ob{k}];\
             [oa{k}][ra{k}]ssim=stats_file={ssim}:shortest=1[vs{k}];\
             [ob{k}][rb{k}]psnr=stats_file={psnr}:shortest=1[vp{k}]",
            ssim = escape_filter_value(&ssim.to_string_lossy()),
            psnr = escape_filter_value(&psnr.to_string_lossy()),
        ));
    }
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
        ref_from,
        ref_to,
        window.source_preroll,
    ));
    args.extend(["-i".to_string(), req.source.to_string_lossy().into_owned()]);
    args.extend(input_window(
        align.output_video_start,
        align.output_format_start,
        out_from,
        out_to,
        output_preroll,
    ));
    args.extend(["-i".to_string(), req.output.to_string_lossy().into_owned()]);
    args.extend(["-filter_complex".to_string(), graph]);
    for k in 0..n {
        args.extend([
            "-map".to_string(),
            format!("[vs{k}]"),
            "-map".to_string(),
            format!("[vp{k}]"),
        ]);
    }
    args.extend(["-f", "null", "-"].iter().map(|s| s.to_string()));
    args
}

/// How the full-length scan lines the files up, using the alignment the
/// segments found and the same trimming convention: both files are cut a
/// quarter frame before the source frame `first_frame`, and output frame
/// `output_skip + i` is paired with reference frame `reference_skip + i`
/// (searched one frame either way).
#[derive(Debug, Clone, Copy, PartialEq)]
struct ScanPlan {
    /// Trim point, seconds from the source's first video frame.
    at: f64,
    /// The alignment guess (seconds) added to `at` for the output.
    shift: f64,
    output_skip: i64,
    reference_skip: i64,
    /// Where scan frame 0 is, in seconds from the source's video start.
    start_offset: f64,
}

impl ScanPlan {
    fn new(align: &Alignment, found: SegmentAlignment) -> Self {
        let fps = align.source_fps;
        // Start where both files have pictures.
        let first_frame = ((-found.shift).max(0.0) * fps).round();
        let at = (first_frame - 0.25) / fps;
        let output_skip = (SCAN_SEARCH_FRAMES - found.frames).max(0);
        let reference_skip = output_skip + found.frames;
        Self {
            at,
            shift: found.shift,
            output_skip,
            reference_skip,
            start_offset: first_frame / fps + reference_skip as f64 / align.rate,
        }
    }
}

/// The ffmpeg command for the full-length scan (see [`ScanPlan`]): both
/// files reduced to [`Alignment::scan_size`], SSIM of each output frame
/// against the reference frames one before, at and one after its match
/// into `paths`, and black/frozen detection on each side (filter instances
/// named `@src` and `@out`).
fn scan_args(
    req: &ValidateRequest<'_>,
    align: &Alignment,
    plan: &ScanPlan,
    paths: &[PathBuf],
) -> Vec<String> {
    let (sw, sh) = align.scan_size;
    let n = paths.len();
    let search = (n as i64 - 1) / 2;
    // With -copyts the trims use each file's own timestamps, whatever
    // tracks ffmpeg would otherwise measure its start from.
    let src_start = align.source_video_start + plan.at;
    let out_start = align.output_video_start + plan.at + plan.shift;
    let detect = |side: &str| {
        format!(
            "blackdetect@{side}={BLACKDETECT_OPTS},{FREEZE_BLUR},\
             freezedetect@{side}={FREEZEDETECT_OPTS},nullsink"
        )
    };
    let labels = |prefix: &str| (0..n).map(|k| format!("[{prefix}{k}]")).collect::<String>();
    let mut graph = format!(
        "{},scale={sw}:{sh}:flags=area,format=yuv420p,split={}{}[sd];\
         [sd]{};\
         [1:{oi}]trim=start={out_start:.6},setpts=PTS-STARTPTS,fps={rate},\
         scale={sw}:{sh}:flags=area,format=yuv420p,split={}{}[od];\
         [od]{}",
        source_chain(align, &format!("trim=start={src_start:.6}")),
        n + 1,
        labels("s"),
        detect("src"),
        n + 1,
        labels("o"),
        detect("out"),
        oi = align.output_index,
        rate = align.rate_expr,
    );
    for (k, path) in paths.iter().enumerate() {
        let reference_skip = (plan.reference_skip + k as i64 - search).max(0);
        graph.push_str(&format!(
            ";[s{k}]trim=start_frame={reference_skip},setpts=PTS-STARTPTS[ss{k}];\
             [o{k}]trim=start_frame={},setpts=PTS-STARTPTS[oo{k}];\
             [oo{k}][ss{k}]ssim=stats_file={}:shortest=1[v{k}]",
            plan.output_skip,
            escape_filter_value(&path.to_string_lossy()),
        ));
    }
    let mut args: Vec<String> = [
        "-nostdin",
        "-hide_banner",
        "-loglevel",
        "level+info",
        "-progress",
        "pipe:1",
        "-nostats",
        "-copyts",
        "-i",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(req.source.to_string_lossy().into_owned());
    args.push("-i".to_string());
    args.push(req.output.to_string_lossy().into_owned());
    args.extend(["-filter_complex".to_string(), graph]);
    for k in 0..n {
        args.extend(["-map".to_string(), format!("[v{k}]")]);
    }
    args.extend(["-f", "null", "-"].iter().map(|s| s.to_string()));
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
            .prefix("szalinski-verify-")
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
pub(crate) enum ProbeError {
    Cancelled,
    Failed(String),
}

/// What verification needs to know about a file.
#[derive(Debug, Clone, Default, PartialEq)]
struct MediaProbe {
    /// The container's length. After [`probe_media`], a length from the
    /// file's start (see [`MediaProbe::settle_untrusted`]).
    duration: Option<f64>,
    start_time: Option<f64>,
    /// A Matroska (or WebM) file.
    matroska: bool,
    /// It states no length that can be trusted and its packets couldn't be
    /// listed, or it states none and stops in the middle of a packet, so
    /// its length is unknown (see [`MediaProbe::settle_untrusted`]).
    length_unknown: bool,
    /// Its packets were listed and ffprobe found it stops in the middle of
    /// one: it was cut short.
    cut_off: bool,
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
    /// Where `duration` came from.
    duration_from: DurationFrom,
}

/// Where a stream's length came from, best first.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum DurationFrom {
    /// No length known.
    #[default]
    Unknown,
    /// ffprobe's own stream duration (MP4 and most containers).
    Stream,
    /// The plain Matroska `DURATION` tag, which ffmpeg's MKV writer adds.
    Tag,
    /// A localized `DURATION-xx` tag: the stream has no other length.
    LocalizedTag,
    /// A tag contradicted by the file's own timing (or not trusted at
    /// all), replaced by the end of the stream's last packet.
    Packets,
    /// A tag contradicted by the file's own timing, replaced by the
    /// container's length.
    Container,
}

/// Why a file's stated lengths are checked against its packets (see
/// [`MediaProbe::length_check`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LengthCheck {
    /// Tag-derived lengths that the container's own length contradicts
    /// (see [`MediaProbe::tag_doubt`]).
    Doubted,
    /// Nothing the file states can be taken as its length: it states no
    /// length but its tags (a Matroska file written as a live stream), or
    /// none at all (the same file without tags, or written through a pipe;
    /// a raw elementary stream; a transport stream or VOB ffprobe couldn't
    /// estimate), or it is a Matroska file whose timestamps don't start at
    /// zero, where ffmpeg's writer states where the file ends rather than
    /// how long it is (and through a pipe, how long it is).
    Untrusted,
}

impl DurationFrom {
    fn is_tag(self) -> bool {
        matches!(self, Self::Tag | Self::LocalizedTag)
    }
}

/// How a tag-derived stream length disagrees with the container's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TagDoubt {
    /// Longer than the container by more than the duration tolerance: no
    /// stream outlasts its container, so the tag is out of date.
    Longer,
    /// A picture or sound track shorter than the container by more than
    /// the duration tolerance: out of date, or a track that really ends
    /// early (the container holds a subtitle that runs on).
    Shorter,
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

impl ProbedStream {
    /// A picture or sound track (cover art is neither).
    fn is_av(&self) -> bool {
        !self.attached_pic && matches!(self.kind, Some(StreamKind::Video | StreamKind::Audio))
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
            .filter(|s| s.is_av())
            .filter_map(|s| s.duration)
            .filter(|d| d.is_finite() && *d > 0.0)
            .reduce(f64::max)
    }

    /// Whether a picture or sound track uses `codec`.
    fn has_codec(&self, codec: &str) -> bool {
        self.kind_of_codec(codec).is_some()
    }

    /// "video" or "audio" for the first picture or sound track using
    /// `codec`.
    fn kind_of_codec(&self, codec: &str) -> Option<&'static str> {
        self.streams
            .iter()
            .filter(|s| !s.attached_pic && s.codec == codec)
            .find_map(|s| match s.kind {
                Some(StreamKind::Video) => Some("video"),
                Some(StreamKind::Audio) => Some("audio"),
                _ => None,
            })
    }

    /// Container duration, else the longest stream.
    fn best_duration(&self) -> Option<f64> {
        self.container_duration().or_else(|| {
            self.streams
                .iter()
                .filter_map(|s| s.duration)
                .filter(|d| d.is_finite() && *d > 0.0)
                .reduce(f64::max)
        })
    }

    fn container_duration(&self) -> Option<f64> {
        self.duration.filter(|d| d.is_finite() && *d > 0.0)
    }

    /// How `stream`'s tag-derived length disagrees with the container's,
    /// if it does by more than the duration tolerance.
    fn tag_doubt(&self, stream: &ProbedStream) -> Option<TagDoubt> {
        if !stream.duration_from.is_tag() {
            return None;
        }
        let container = self.container_duration()?;
        let tag = stream.duration?;
        let tolerance = duration_tolerance(container);
        if tag > container + tolerance {
            Some(TagDoubt::Longer)
        } else if stream.is_av() && tag + tolerance < container {
            Some(TagDoubt::Shorter)
        } else {
            None
        }
    }

    /// Whether any stream's length comes from a Matroska tag that the
    /// container's own length contradicts.
    fn has_doubtful_tags(&self) -> bool {
        self.streams.iter().any(|s| self.tag_doubt(s).is_some())
    }

    /// Where the file's timestamps start (see [`timeline::start_offset`]):
    /// lengths are counted from here.
    fn start_offset(&self) -> f64 {
        timeline::start_offset(self.start_time)
    }

    /// Whether the lengths the file states need checking against where its
    /// packets end, and why (see [`LengthCheck`]).
    fn length_check(&self) -> Option<LengthCheck> {
        let shifted = self.matroska && self.start_offset() > 0.0;
        let no_container = self.container_duration().is_none();
        let tags_only = no_container
            && self
                .streams
                .iter()
                .any(|s| s.is_av() && s.duration_from.is_tag() && s.duration.is_some());
        // Neither the container nor a picture or sound stream says how
        // long it is: in any container, only its packets can tell.
        let unstated = no_container && self.av_duration().is_none();
        if shifted || tags_only || unstated {
            Some(LengthCheck::Untrusted)
        } else if self.has_doubtful_tags() {
            Some(LengthCheck::Doubted)
        } else {
            None
        }
    }

    /// Where to start listing packets to see the last seconds of the file
    /// (see [`timeline::tail_start`]).
    fn tail_start(&self) -> Option<f64> {
        let tags: Vec<f64> = self
            .streams
            .iter()
            .filter(|s| s.duration_from.is_tag())
            .filter_map(|s| s.duration)
            .collect();
        timeline::tail_start(self.start_offset(), self.container_duration(), &tags)
    }

    /// Replace tag-derived stream lengths the container contradicts with
    /// the file's real timing. `tail` (when it could be read) tells where
    /// each stream's last packet ends: a stream with packets there gets
    /// that length. A picture or sound track with none ended before the
    /// part read: its shorter tag stands when it says so (a subtitle that
    /// runs on keeps the container longer), anything else takes the
    /// container's length. Without `tail`, or when the file was cut off
    /// (its packets then show where it stops, not how long it should be),
    /// every contradicted tag takes the container's length. Nothing changes
    /// when the container has no length to compare with.
    fn settle_tagged_durations(&mut self, tail: Option<&PacketEnds>) {
        let Some(container) = self.container_duration() else {
            return;
        };
        let start = self.start_offset();
        let tail = tail.filter(|t| !t.cut_off);
        let read_from = tail.and_then(|t| t.first).map(|f| f - start);
        let tolerance = duration_tolerance(container);
        let settled: Vec<(usize, Option<(f64, DurationFrom)>)> = self
            .streams
            .iter()
            .enumerate()
            .filter_map(|(i, stream)| {
                let doubt = self.tag_doubt(stream)?;
                let tag = stream.duration?;
                let measured = tail.and_then(|t| {
                    t.ends
                        .get(&stream.index)
                        .map(|end| timeline::since_start(*end, start))
                        .filter(|d| d.is_finite() && *d > 0.0)
                });
                let ended_before_tail = read_from.is_some_and(|from| tag <= from + tolerance);
                let replacement = match measured {
                    Some(secs) => Some((secs, DurationFrom::Packets)),
                    None if doubt == TagDoubt::Shorter && ended_before_tail => None,
                    None => Some((container, DurationFrom::Container)),
                };
                Some((i, replacement))
            })
            .collect();
        for (i, replacement) in settled {
            let Some(stream) = self.streams.get_mut(i) else {
                continue;
            };
            match replacement {
                Some((secs, from)) => {
                    tracing::debug!(
                        stream = stream.index,
                        tagged = ?stream.duration,
                        container,
                        "a stream's DURATION tag disagrees with the file; using {secs:.3} s ({from:?})"
                    );
                    stream.duration = Some(secs);
                    stream.duration_from = from;
                }
                None => tracing::debug!(
                    stream = stream.index,
                    tagged = ?stream.duration,
                    container,
                    "a track ends before the container; its DURATION tag agrees with its packets"
                ),
            }
        }
    }

    /// Settle the lengths of a file whose stated lengths can't be trusted
    /// ([`LengthCheck::Untrusted`]) by `packets`, the end of its packet
    /// listing (or the whole of it), counting from where the file starts:
    ///
    /// - Listed: the container's length is where the last packet ends, and
    ///   every stream with packets listed is as long as they reach. A
    ///   tag-derived picture or sound track with none ended before the part
    ///   listed: its tag, read as a length, stands if it says so, else its
    ///   length is unknown (a subtitle keeps its tag, read as a length; it
    ///   isn't compared).
    /// - Cut off (ffprobe found the file stops in the middle of a packet):
    ///   the packets show where it stops, not how long it should be, so
    ///   what it states stands, read as lengths (see
    ///   [`timeline::stated_length`]); an original cut short this way is
    ///   still found to be shorter than it says. One that states nothing
    ///   has no length then: unknown.
    /// - Not listed: the lengths are unknown, except a container length
    ///   smaller than the start time, which can only be a length.
    fn settle_untrusted(&mut self, packets: Option<&PacketEnds>) {
        let start = self.start_offset();
        let stated = self.duration;
        let as_length = |d: f64| timeline::stated_length(d, start);
        match packets {
            Some(p) if p.cut_off => {
                self.duration = stated.map(as_length);
                for stream in &mut self.streams {
                    if stream.duration_from.is_tag() {
                        stream.duration = stream.duration.map(as_length);
                    }
                }
                self.cut_off = true;
                self.length_unknown = self.best_duration().is_none();
                tracing::debug!(
                    ?stated,
                    "the file stops in the middle of a packet; keeping the lengths it states"
                );
            }
            Some(p) if p.length(start).is_some() => {
                let length = p.length(start);
                let tolerance = duration_tolerance(length.unwrap_or_default());
                let listed_from = p.first.map(|f| f - start);
                self.duration = length;
                for stream in &mut self.streams {
                    if let Some(end) = p.ends.get(&stream.index) {
                        let secs = timeline::since_start(*end, start);
                        if secs.is_finite() && secs > 0.0 {
                            stream.duration = Some(secs);
                            stream.duration_from = DurationFrom::Packets;
                        }
                    } else if stream.duration_from.is_tag() {
                        // A subtitle line listed before may last past the
                        // part listed; picture and sound packets are short.
                        let tagged = stream.duration.map(as_length).filter(|l| {
                            !stream.is_av()
                                || listed_from.is_some_and(|from| *l <= from + tolerance)
                        });
                        if tagged.is_none() {
                            stream.duration_from = DurationFrom::Unknown;
                        }
                        stream.duration = tagged;
                    }
                }
                tracing::debug!(
                    ?stated,
                    start,
                    "the file's stated lengths can't be trusted; its packets make it {:.3} s long",
                    length.unwrap_or_default()
                );
            }
            _ => {
                self.duration = stated.filter(|d| start > 0.0 && *d < start);
                for stream in &mut self.streams {
                    if stream.duration_from.is_tag() {
                        stream.duration = None;
                        stream.duration_from = DurationFrom::Unknown;
                    }
                }
                self.length_unknown = self.best_duration().is_none();
                tracing::debug!(
                    ?stated,
                    "the file's stated lengths can't be trusted and its packets couldn't be \
                     listed; its length is unknown"
                );
            }
        }
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
    /// Matroska keeps per-stream durations in `DURATION` tags.
    #[serde(default)]
    tags: BTreeMap<String, String>,
}

impl FfprobeStream {
    /// ffprobe's own stream duration, else the Matroska `DURATION` tags:
    /// the plain tag ffmpeg writes, and a localized one (`DURATION-eng`,
    /// possibly left over from before a cut) only when there is no other
    /// (see [`szalinski_core::tags`]).
    fn duration_secs(&self) -> (Option<f64>, DurationFrom) {
        if let Some(secs) = parse_seconds(self.duration.as_ref()).filter(|d| *d > 0.0) {
            return (Some(secs), DurationFrom::Stream);
        }
        let pairs = self.tags.iter().map(|(k, v)| (k.as_str(), v.as_str()));
        match szalinski_core::tags::tagged_duration(pairs) {
            Some(tag) if tag.localized => (Some(tag.value), DurationFrom::LocalizedTag),
            Some(tag) => (Some(tag.value), DurationFrom::Tag),
            None => (None, DurationFrom::Unknown),
        }
    }
}

#[derive(Debug, Deserialize)]
struct FfprobeDisposition {
    #[serde(default)]
    attached_pic: i64,
}

#[derive(Debug, Deserialize)]
struct FfprobeFormat {
    format_name: Option<String>,
    duration: Option<String>,
    start_time: Option<String>,
}

fn parse_seconds(value: Option<&String>) -> Option<f64> {
    value?.trim().parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Parse `ffprobe -print_format json -show_format -show_streams` output.
fn parse_probe_json(json: &[u8]) -> Result<MediaProbe, String> {
    let raw: FfprobeJson = serde_json::from_slice(json).map_err(|e| {
        tracing::debug!("ffprobe's answer could not be read: {e}");
        "ffprobe's answer about the file couldn't be read".to_string()
    })?;
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
            let (duration, duration_from) = s.duration_secs();
            ProbedStream {
                duration,
                duration_from,
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
    let matroska = format
        .as_ref()
        .and_then(|f| f.format_name.as_deref())
        .is_some_and(|name| {
            name.split(',')
                .any(|n| matches!(n.trim(), "matroska" | "webm"))
        });
    Ok(MediaProbe {
        duration: format
            .as_ref()
            .and_then(|f| parse_seconds(f.duration.as_ref())),
        start_time: format
            .as_ref()
            .and_then(|f| parse_seconds(f.start_time.as_ref())),
        matroska,
        length_unknown: false,
        cut_off: false,
        streams,
    })
}

/// Probe `path` for verification: ffprobe's description of it (killed
/// after [`PROBE_TIMEOUT`] or on cancel), with lengths counted from where
/// the file starts. Lengths it states that can't be taken as they are (see
/// [`MediaProbe::length_check`]) are checked against where the streams'
/// packets really end: the last seconds of the file are listed (no
/// decoding), and when that lists nothing, or the file states no length to
/// read back from, all of it (see [`MediaProbe::settle_tagged_durations`]
/// and [`MediaProbe::settle_untrusted`]). An out-of-date tag, a file that
/// states no length but its tags or none at all, or timestamps that don't
/// start at zero can then neither make a new file look as long as an
/// original it doesn't match, nor make a complete one look cut short. The
/// new file is read the same way.
async fn probe_media(
    ffprobe: &Path,
    path: &Path,
    cancel: &CancellationToken,
) -> Result<MediaProbe, ProbeError> {
    probe_and_list(ffprobe, path, cancel)
        .await
        .map(|probed| probed.probe)
}

/// What [`probe_and_list`] found.
struct Probed {
    probe: MediaProbe,
    /// Why the lengths it states were checked against its packets, if they
    /// were (an untrusted length lists every packet when its last ones
    /// can't be).
    check: Option<LengthCheck>,
    /// The packets listed for that, when they could be listed.
    packets: Option<PacketEnds>,
}

/// [`probe_media`], with the packets it listed to settle the file's
/// lengths.
async fn probe_and_list(
    ffprobe: &Path,
    path: &Path,
    cancel: &CancellationToken,
) -> Result<Probed, ProbeError> {
    let args = [
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ];
    let json = run_ffprobe(ffprobe, &args, path, cancel).await?;
    let mut probe = parse_probe_json(&json).map_err(ProbeError::Failed)?;
    let Some(check) = probe.length_check() else {
        return Ok(Probed {
            probe,
            check: None,
            packets: None,
        });
    };
    let mut packets = match probe.tail_start() {
        Some(from) => listing(
            list_packets(ffprobe, path, Some(from), cancel).await,
            path,
            "the last packets",
        )?,
        None => None,
    };
    if check == LengthCheck::Untrusted && packets.as_ref().is_none_or(|p| p.end().is_none()) {
        // It can't be read from near its end, or states no length to read
        // back from, and nothing else tells its length: every packet is
        // listed (no decoding, but the whole file is read: about 5 s per
        // GB, and only for these rare files).
        tracing::debug!(file = %path.display(), "listing every packet to find where the file ends");
        packets = listing(
            list_packets(ffprobe, path, None, cancel).await,
            path,
            "the file's packets",
        )?;
    }
    match check {
        LengthCheck::Doubted => probe.settle_tagged_durations(packets.as_ref()),
        LengthCheck::Untrusted => probe.settle_untrusted(packets.as_ref()),
    }
    Ok(Probed {
        probe,
        check: Some(check),
        packets,
    })
}

/// A packet listing, or `None` when ffprobe couldn't make one (why is
/// logged); only cancelling is an error.
fn listing(
    result: Result<PacketEnds, ProbeError>,
    path: &Path,
    what: &str,
) -> Result<Option<PacketEnds>, ProbeError> {
    match result {
        Ok(ends) => Ok(Some(ends)),
        Err(ProbeError::Cancelled) => Err(ProbeError::Cancelled),
        Err(ProbeError::Failed(why)) => {
            tracing::debug!(file = %path.display(), "could not list {what}: {why}");
            Ok(None)
        }
    }
}

/// Where an original's packets end (see [`source_length`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum PacketsEnd {
    /// They couldn't be listed (ffprobe failed on them, or listed nothing
    /// new for [`PROBE_TIMEOUT`]), or none of them had a time.
    Unlisted,
    /// ffprobe found the file stops in the middle of a packet: it was cut
    /// short, whatever it states. The last packet listed ends `at` seconds
    /// after the file's start.
    CutOff { at: Option<f64> },
    /// The last packet ends this many seconds after the file's start.
    At(f64),
}

/// How long an original is, and where its packets really end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SourceLength {
    /// Its length, read the way verification reads it (see
    /// [`probe_media`]): its picture and sound, else its container's.
    /// `None` when it can't be told.
    pub(crate) length: Option<f64>,
    /// Where its packets end.
    pub(crate) packets: PacketsEnd,
}

/// How long the original at `path` is, read the way verification reads it
/// (see [`probe_media`]), and where its packets really end: the packets
/// listed to read its length when it was measured that way, else those of
/// its last seconds, listed from the end it states (no decoding), and when
/// it states none or nothing was listed from there, all of them (never a
/// listing reading its length already tried). An original whose packets
/// reach its length is complete, whatever a conversion made of it (see
/// `run::early_end`).
pub(crate) async fn source_length(
    ffprobe: &Path,
    path: &Path,
    cancel: &CancellationToken,
) -> Result<SourceLength, ProbeError> {
    let Probed {
        probe,
        check,
        packets: measured,
    } = probe_and_list(ffprobe, path, cancel).await?;
    let length = probe.av_duration().or_else(|| probe.best_duration());
    let mut packets = measured.filter(|p| p.cut_off || p.end().is_some());
    if packets.is_none()
        && check.is_none()
        && let Some(from) = probe.tail_start()
    {
        packets = listing(
            list_packets(ffprobe, path, Some(from), cancel).await,
            path,
            "the last packets",
        )?
        .filter(|p| p.end().is_some());
    }
    if packets.is_none() && check != Some(LengthCheck::Untrusted) {
        packets = listing(
            list_packets(ffprobe, path, None, cancel).await,
            path,
            "the file's packets",
        )?;
    }
    let start = probe.start_offset();
    let end = |p: &PacketEnds| p.end().map(|end| timeline::since_start(end, start));
    let packets = match packets {
        Some(p) if p.cut_off => PacketsEnd::CutOff { at: end(&p) },
        Some(p) => end(&p).map_or(PacketsEnd::Unlisted, PacketsEnd::At),
        None => PacketsEnd::Unlisted,
    };
    Ok(SourceLength { length, packets })
}

/// List the packets of `path` (no decoding) from `from` seconds on, or all
/// of them, keeping where each stream's last one ends (see [`PacketEnds`]).
/// The listing is read line by line as ffprobe prints it, so even a whole
/// film's is never held in memory. ffprobe is killed on cancel, or when it
/// lists nothing new for [`PROBE_TIMEOUT`] (a seek that has to read
/// through a large file without an index, or a share that stopped
/// answering).
async fn list_packets(
    ffprobe: &Path,
    path: &Path,
    from: Option<f64>,
    cancel: &CancellationToken,
) -> Result<PacketEnds, ProbeError> {
    let interval = from.map(|f| format!("{f:.3}%"));
    let mut args = vec!["-v", "error"];
    if let Some(interval) = interval.as_deref() {
        args.extend(["-read_intervals", interval]);
    }
    args.extend(["-show_entries", PACKET_ENTRIES, "-of", "compact=p=0"]);
    let mut child = spawn_ffprobe(ffprobe, &args, path, cancel).await?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(ProbeError::Failed(
            "ffprobe's output couldn't be read".to_string(),
        ));
    };
    let mut ends = PacketEnds::default();
    let listing = async {
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        loop {
            match tokio::time::timeout(PROBE_TIMEOUT, lines.next_line()).await {
                Err(_) => {
                    return Err(format!(
                        "ffprobe listed nothing for {} seconds",
                        PROBE_TIMEOUT.as_secs()
                    ));
                }
                Ok(Ok(Some(line))) => ends.push_line(&line),
                Ok(Ok(None)) => return Ok(()),
                Ok(Err(e)) => {
                    tracing::debug!("ffprobe's packet listing couldn't be read: {e}");
                    return Err("ffprobe stopped unexpectedly".to_string());
                }
            }
        }
    };
    // ffprobe's messages are read alongside; they end when it does (it is
    // killed when this returns early).
    let messages = tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(stderr).lines();
        let (mut cut_off, mut first) = (false, None::<String>);
        while let Ok(Some(line)) = lines.next_line().await {
            let message = crate::ffmpeg::strip_log_prefix(&line).trim();
            cut_off |= timeline::is_cut_off_message(message);
            if first.is_none() && !message.is_empty() {
                first = Some(message.to_string());
            }
        }
        (cut_off, first)
    });
    let listed = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProbeError::Cancelled),
        listed = listing => listed,
    };
    listed.map_err(ProbeError::Failed)?;
    let status = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProbeError::Cancelled),
        status = tokio::time::timeout(PROBE_TIMEOUT, child.wait()) => status,
    };
    let (cut_off, first_message) = match tokio::time::timeout(PROBE_TIMEOUT, messages).await {
        Ok(Ok(read)) => read,
        _ => (false, None),
    };
    ends.cut_off = cut_off;
    match status {
        Ok(Ok(status)) if status.success() => Ok(ends),
        // A cut-off file may end the listing with an error; what was
        // listed until then still shows where it stops.
        Ok(Ok(_)) if ends.cut_off && ends.end().is_some() => Ok(ends),
        Ok(Ok(_)) => {
            Err(ProbeError::Failed(first_message.unwrap_or_else(|| {
                "it is not a readable media file".to_string()
            })))
        }
        Ok(Err(e)) => {
            tracing::debug!("ffprobe stopped unexpectedly: {e}");
            Err(ProbeError::Failed(
                "ffprobe stopped unexpectedly".to_string(),
            ))
        }
        Err(_) => Err(ProbeError::Failed(format!(
            "ffprobe didn't finish within {} seconds",
            PROBE_TIMEOUT.as_secs()
        ))),
    }
}

/// Start ffprobe with `args` then `path`, its output piped (killed when
/// dropped, or with the server).
async fn spawn_ffprobe(
    ffprobe: &Path,
    args: &[&str],
    path: &Path,
    cancel: &CancellationToken,
) -> Result<tokio::process::Child, ProbeError> {
    let mut command = tokio::process::Command::new(ffprobe);
    command
        .args(args)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    szalinski_core::process::end_with_parent(command.as_std_mut());
    let spawned = crate::ffmpeg::spawn_patiently(
        || command.spawn(),
        &szalinski_core::process::SPAWN_RETRY_DELAYS,
        cancel,
    )
    .await;
    match spawned {
        Ok(Some(child)) => Ok(child),
        Ok(None) => Err(ProbeError::Cancelled),
        Err(e) => Err(ProbeError::Failed(
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("ffprobe was not found at {}", ffprobe.display())
            } else {
                format!(
                    "ffprobe couldn't be started because {}",
                    szalinski_core::plain::io_reason(&e)
                )
            },
        )),
    }
}

/// Run ffprobe with `args` then `path` and return what it printed (killed
/// after [`PROBE_TIMEOUT`] or on cancel).
async fn run_ffprobe(
    ffprobe: &Path,
    args: &[&str],
    path: &Path,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, ProbeError> {
    let child = spawn_ffprobe(ffprobe, args, path, cancel).await?;
    let output = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ProbeError::Cancelled),
        result = tokio::time::timeout(PROBE_TIMEOUT, child.wait_with_output()) => match result {
            Err(_) => return Err(ProbeError::Failed(format!(
                "ffprobe gave no answer within {} seconds", PROBE_TIMEOUT.as_secs()
            ))),
            Ok(Err(e)) => {
                tracing::debug!("ffprobe stopped unexpectedly: {e}");
                return Err(ProbeError::Failed("ffprobe stopped unexpectedly".to_string()));
            }
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
    Ok(output.stdout)
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

/// A size as the job sheet and the rest of the app show it (`3.13 MB`,
/// `572 KB`): the shared formatter in `szalinski-core`.
pub(crate) fn human_bytes(bytes: u64) -> String {
    szalinski_core::format::bytes(bytes)
}

/// SSIM and PSNR statistics files for one segment, per searched offset.
fn stats_paths(
    dir: &Path,
    index: usize,
    offsets: std::ops::RangeInclusive<i64>,
) -> Vec<(i64, PathBuf, PathBuf)> {
    offsets
        .map(|j| {
            (
                j,
                dir.join(format!("seg{index}_{j}_ssim.log")),
                dir.join(format!("seg{index}_{j}_psnr.log")),
            )
        })
        .collect()
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
    fn detect_intervals_from_log_lines() {
        let lines = [
            "[blackdetect@out @ 0x7f10d4000e40] [info] black_start:6 black_end:10.958 black_duration:4.958",
            "[freezedetect@out @ 0x55] [info] lavfi.freezedetect.freeze_start: 3.065",
            "[freezedetect@out @ 0x55] [info] lavfi.freezedetect.freeze_duration: 2",
            "[freezedetect@out @ 0x55] [info] lavfi.freezedetect.freeze_end: 5.065",
            "[freezedetect@out @ 0x55] [info] lavfi.freezedetect.freeze_start: 9",
            "[info] unrelated line",
        ];
        let mut log = DetectLog::default();
        for l in lines {
            let (context, _, message) = split_log_line(l);
            if context.is_some_and(|c| c.ends_with("@out")) {
                log.push(message);
            }
        }
        let log = log.finish(11.0);
        assert_eq!(log.black, [(6.0, 10.958)]);
        // A closed freeze, and one still frozen at the end of the stream.
        assert_eq!(log.frozen, [(3.065, 5.065), (9.0, 11.0)]);
    }

    #[test]
    fn added_time_counts_only_new_intervals() {
        // Same black stretch in both (boundaries jitter): nothing added.
        let pass = added_time_check(&[(10.0, 14.0)], &[(10.1, 14.2)], "black");
        assert_eq!(pass.status, CheckStatus::Pass);
        assert_eq!(
            pass.detail,
            "Black video: 4.0 s in the original, 4.1 s in the new file"
        );
        // Equal totals but at a different place: that is added freezing.
        let moved = added_time_check(&[(10.0, 14.0)], &[(30.0, 34.0)], "frozen");
        assert_eq!(moved.status, CheckStatus::Fail);
        assert_eq!(
            moved.detail,
            "The new file has 4.0 s more frozen video than the original, starting near 30.0 s"
        );
        assert_eq!(
            subtract_intervals(&[(0.0, 10.0)], &[(2.0, 3.0), (5.0, 6.0), (9.0, 12.0)]),
            [(0.0, 2.0), (3.0, 5.0), (6.0, 9.0)]
        );
    }

    #[test]
    fn damage_includes_concealment_and_corrupt_frame_warnings() {
        let mut damage = DamageLog::default();
        damage.push("[h264 @ 0x56] [info] Some harmless notice");
        damage.push("[aac @ 0x57] [warning] If you heard an audible artifact, there may be a bug");
        assert!(damage.is_empty());
        damage.push("[vist#0:0/h264 @ 0x56] [warning] corrupt decoded frame");
        damage.push("[h264 @ 0x56] [info] concealing 451 DC, 451 AC, 451 MV errors in B frame");
        damage.push("[h264 @ 0x56] [error] cbp too large (353) at 76 4");
        damage.push("[aist#0:1/ac3 @ 0x58] [error] expacc 127 is out-of-range");
        damage.push("[in#0/matroska,webm @ 0x59] [error] Invalid length 0x1ae2");
        assert_eq!(damage.count("h264"), 3);
        assert_eq!(damage.count("ac3"), 1);
        assert_eq!(damage.count("matroska,webm"), 1);
        assert_eq!(
            damage.by_decoder["h264"].first,
            "corrupt decoded frame (h264)"
        );
    }

    fn decode_result(lines: &[&str]) -> DecodeResult {
        let mut damage = DamageLog::default();
        for l in lines {
            damage.push(l);
        }
        DecodeResult {
            exit: FfmpegExit::Success {
                tail: String::new(),
            },
            damage,
            decoded_secs: Some(10.0),
        }
    }

    #[test]
    fn inherited_damage_warns_and_new_damage_fails() {
        let decode = decode_result(&[
            "[aist#0:1/ac3 @ 0x58] [error] expacc 127 is out-of-range",
            "[ac3 @ 0x58] [error] error decoding the audio block",
        ]);
        let mut inherited = Inherited::default();
        let fail = decode_check(&decode, Some(10.0), &inherited);
        assert_eq!(fail.status, CheckStatus::Fail);
        assert_eq!(
            fail.detail,
            "Found a playback error: expacc 127 is out-of-range (ac3) (1 more)"
        );

        inherited.decoders.insert("ac3".into(), ("audio", 165));
        let warn = decode_check(&decode, Some(10.0), &inherited);
        assert_eq!(warn.status, CheckStatus::Warn);
        assert_eq!(
            warn.detail,
            "The original's audio (ac3) already had playback errors; the new file has the same ones and no new ones"
        );

        // New damage in another track still fails.
        let mixed = decode_result(&[
            "[ac3 @ 0x58] [error] error decoding the audio block",
            "[vist#0:0/h264 @ 0x56] [warning] corrupt decoded frame",
        ]);
        let fail = decode_check(&mixed, Some(10.0), &inherited);
        assert_eq!(fail.status, CheckStatus::Fail);
        assert!(fail.detail.contains("(h264)"), "{}", fail.detail);

        let clean = decode_result(&[]);
        let pass = decode_check(&clean, Some(10.0), &Inherited::default());
        assert_eq!(pass.status, CheckStatus::Pass);
        assert_eq!(pass.detail, "Decoded all 10.0 s without errors");
    }

    #[test]
    fn decoder_keys() {
        assert_eq!(decoder_key(Some("vist#0:0/h264")), "h264");
        assert_eq!(decoder_key(Some("h264")), "h264");
        assert_eq!(decoder_key(Some("in#0/matroska,webm")), "matroska,webm");
        assert_eq!(decoder_key(None), "");
    }

    #[test]
    fn describes_stream_counts() {
        let c = |video, audio, subtitle| StreamSummary {
            video,
            audio,
            subtitle,
        };
        assert_eq!(describe_counts(c(1, 0, 0)), "1 video track");
        assert_eq!(
            describe_counts(c(1, 1, 0)),
            "1 video track and 1 audio track"
        );
        assert_eq!(
            describe_counts(c(1, 2, 1)),
            "1 video track, 2 audio tracks and 1 subtitle track"
        );
        assert_eq!(
            describe_counts(c(2, 0, 3)),
            "2 video tracks, 0 audio tracks and 3 subtitle tracks"
        );
    }

    #[test]
    fn duration_tolerance_is_one_second_or_half_a_percent() {
        assert!(duration_check(Some(6.0), Some(6.9)).status == CheckStatus::Pass);
        assert!(duration_check(Some(6.0), Some(3.0)).status == CheckStatus::Fail);
        // 2 h: 0.5 % is 36 s.
        assert!(duration_check(Some(7200.0), Some(7170.0)).status == CheckStatus::Pass);
        assert!(duration_check(Some(7200.0), Some(7150.0)).status == CheckStatus::Fail);
        assert_eq!(duration_check(Some(1.0), None).status, CheckStatus::Fail);
        // An original of unknown length can't vouch for any new file. This
        // was skipped, which let a conversion cut to 30 s replace a 61 s
        // live-stream MKV.
        let unknown = duration_check(None, Some(30.0));
        assert_eq!(unknown.status, CheckStatus::Fail);
        assert_eq!(
            unknown.detail,
            "The original's length couldn't be read, so the new file couldn't be checked against \
             it"
        );
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

    fn seg(start: f64, ssim: Vec<f64>) -> SegmentStats {
        SegmentStats {
            start,
            mse: vec![1.0; ssim.len()],
            ssim,
            alignment: SegmentAlignment {
                shift: 0.0,
                frames: 0,
            },
        }
    }

    #[test]
    fn ssim_thresholds() {
        let good = summarize_segments(&[seg(1.0, vec![0.99, 0.98]), seg(5.0, vec![0.97])]);
        assert_eq!(good.result.status, CheckStatus::Pass);
        assert_eq!(good.ssim_min, Some(0.97));
        assert_eq!(good.alignments.len(), 2);

        let corrupt_frame = summarize_segments(&[seg(1.0, vec![0.99, 0.55, 0.99])]);
        assert_eq!(corrupt_frame.result.status, CheckStatus::Fail);

        let bad_segment = summarize_segments(&[seg(1.0, vec![0.99]), seg(3.0, vec![0.8, 0.82])]);
        assert_eq!(bad_segment.result.status, CheckStatus::Fail);
        assert!(bad_segment.result.detail.contains("3.0 s"));

        let soft = summarize_segments(&[seg(1.0, vec![0.90, 0.91])]);
        assert_eq!(soft.result.status, CheckStatus::Warn);
    }

    fn scan(frames: Vec<f64>) -> ScanOutcome {
        ScanOutcome {
            frames,
            rate: 24.0,
            start_offset: 0.0,
            source: DetectLog::default(),
            output: DetectLog::default(),
        }
    }

    #[test]
    fn full_length_scan_catches_bursts_but_not_single_frames() {
        // Clean: a few isolated dips (scene cuts) are fine.
        let mut clean = vec![0.99; 480];
        clean[100] = 0.4;
        clean[300] = 0.5;
        clean[301] = 0.55;
        assert_eq!(judge_scan(&scan(clean.clone())), None);

        // Garbage frames in a row.
        let mut garbage = clean.clone();
        for v in &mut garbage[200..204] {
            *v = 0.3;
        }
        let ((first, count, low), kind) = judge_scan(&scan(garbage)).unwrap();
        assert_eq!((first, count, kind), (200, 4, "garbage"));
        assert!((low - 0.3).abs() < 1e-9);

        // A 1.4 s half-green burst: ~0.81 per frame at low resolution.
        let mut burst = clean.clone();
        for v in &mut burst[240..274] {
            *v = 0.81;
        }
        let (_, kind) = judge_scan(&scan(burst)).unwrap();
        assert_eq!(kind, "mismatch");

        // A shorter, milder burst (0.6 s at 0.88) in an otherwise ~0.99 file.
        let mut mild = clean.clone();
        for v in &mut mild[240..255] {
            *v = 0.88;
        }
        let ((first, count, _), kind) = judge_scan(&scan(mild)).unwrap();
        assert_eq!((first, count, kind), (240, 15, "burst"));

        // A uniformly softer (grainy, low-bitrate) file with ordinary dips.
        let soft: Vec<f64> = (0..480)
            .map(|i| if i % 50 < 10 { 0.90 } else { 0.95 })
            .collect();
        assert_eq!(judge_scan(&scan(soft)), None);
    }

    #[test]
    fn scan_failures_name_the_time() {
        let mut frames = vec![0.99; 480];
        for v in &mut frames[240..280] {
            *v = 0.2;
        }
        let align = test_alignment();
        let result = merge_scan(
            CheckResult::pass("Matches the original at 10 points", Some(0.99)),
            &scan(frames),
            &align,
        );
        assert_eq!(result.status, CheckStatus::Fail);
        assert_eq!(
            result.detail,
            "The picture from 10.0 s to 11.7 s is damaged (20.0% similar)"
        );
        let result = merge_scan(
            CheckResult::pass("Matches the original at 10 points", Some(0.99)),
            &scan(vec![0.99; 100]),
            &align,
        );
        assert_eq!(result.status, CheckStatus::Pass);
        assert_eq!(
            result.detail,
            "Matches the original at 10 points, and all 100 frames match at low resolution"
        );
    }

    #[test]
    fn best_frame_across_offsets() {
        let per_offset = vec![vec![0.2, 0.9, 0.5], vec![0.8, 0.3], vec![0.1, 0.1, 0.95]];
        assert_eq!(best_per_frame(&per_offset), [0.8, 0.9, 0.95]);
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[]), None);
    }

    fn test_alignment() -> Alignment {
        Alignment {
            source_index: 0,
            output_index: 0,
            source_video_start: 0.0,
            output_video_start: 0.0,
            source_format_start: 0.0,
            output_format_start: 0.0,
            timeline_shift: 0.0,
            av_timeline_shift: 0.0,
            width: 1280,
            height: 720,
            scale_source: false,
            compare_size: (640, 360),
            scan_size: (320, 180),
            rate_expr: "24/1".into(),
            rate: 24.0,
            source_fps: 24.0,
            deinterlace: None,
        }
    }

    #[test]
    fn seek_points_fall_between_frames() {
        let align = test_alignment();
        let at = align.seek_point(1.35);
        // 1.35 s ≈ frame 32; seek a quarter frame before it.
        assert!((at - (32.0 - 0.25) / 24.0).abs() < 1e-9);
        assert_eq!(align.seek_point(0.0), 0.0);
        assert_eq!(align.search_before(at), ALIGN_SEARCH_FRAMES);
        assert_eq!(align.search_before(0.05), 1);
    }

    #[test]
    fn reduced_sizes_keep_the_aspect_ratio() {
        assert_eq!(reduced_size(1920, 1080, COMPARE_PIXELS), (640, 360));
        assert_eq!(reduced_size(3840, 2160, SCAN_PIXELS), (320, 180));
        assert_eq!(reduced_size(720, 576, COMPARE_PIXELS), (536, 430));
        // Never scaled up; odd sizes become even.
        assert_eq!(reduced_size(639, 359, COMPARE_PIXELS), (640, 360));
        assert_eq!(reduced_size(320, 240, COMPARE_PIXELS), (320, 240));
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

    // -----------------------------------------------------------------------
    // Out-of-date Matroska DURATION tags (a clip cut from a film keeps the
    // film's `DURATION-eng` next to ffmpeg's own `DURATION`).

    /// A Matroska probe: video and two audio tracks with `tags`, a subtitle
    /// with `sub_tags`, and the container's length.
    fn mkv_json(tags: &str, sub_tags: &str, container: &str) -> Vec<u8> {
        mkv_json_at(tags, sub_tags, Some(container), 0.0)
    }

    /// [`mkv_json`] with timestamps from `start`, and maybe no container
    /// length (a file written as a live stream).
    fn mkv_json_at(tags: &str, sub_tags: &str, container: Option<&str>, start: f64) -> Vec<u8> {
        let duration = container.map_or(String::new(), |d| format!(r#""duration":"{d}","#));
        format!(
            r#"{{"streams":[
            {{"index":0,"codec_type":"video","codec_name":"hevc","width":3840,"height":2160,
             "avg_frame_rate":"24000/1001","start_time":"{start:.6}","tags":{{{tags}}}}},
            {{"index":1,"codec_type":"audio","codec_name":"truehd","tags":{{{tags}}}}},
            {{"index":2,"codec_type":"audio","codec_name":"ac3","tags":{{{tags}}}}},
            {{"index":3,"codec_type":"subtitle","codec_name":"hdmv_pgs_subtitle",
             "tags":{{{sub_tags}}}}}],
            "format":{{"format_name":"matroska,webm",{duration}"start_time":"{start:.6}"}}}}"#
        )
        .into_bytes()
    }

    const FRESH: &str = r#""DURATION":"00:01:01.061000000""#;
    const STALE: &str = r#""DURATION-eng":"02:21:02.000000000""#;
    const FILM_SECS: f64 = 8462.0;

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-3)
    }

    /// The owner's clip: whichever order the tags come in, every stream's
    /// length is ffmpeg's plain `DURATION`, which the container confirms.
    #[test]
    fn the_plain_duration_tag_wins_over_a_stale_localized_one_in_either_order() {
        for tags in [
            format!(r#"{FRESH},"BPS-eng":"60000000",{STALE}"#),
            format!(r#"{STALE},"BPS-eng":"60000000",{FRESH}"#),
        ] {
            for _ in 0..8 {
                let probe = parse_probe_json(&mkv_json(&tags, &tags, "61.061000")).unwrap();
                for stream in &probe.streams {
                    assert!(close(stream.duration, 61.061), "{stream:?}");
                    assert_eq!(stream.duration_from, DurationFrom::Tag);
                }
                assert!(!probe.has_doubtful_tags());
                assert!(close(probe.av_duration(), 61.061));
                assert!(close(probe.best_duration(), 61.061));
            }
        }
    }

    #[test]
    fn localized_tags_count_when_there_is_nothing_else() {
        let tags = r#""DURATION-eng":"00:01:01.000000000""#;
        let probe = parse_probe_json(&mkv_json(tags, tags, "61.061000")).unwrap();
        let video = probe.primary_video().unwrap();
        assert!(close(video.duration, 61.0));
        assert_eq!(video.duration_from, DurationFrom::LocalizedTag);
        assert!(!probe.has_doubtful_tags(), "they agree with the container");

        // ffprobe's own stream duration (MP4) always comes first.
        let json = br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264",
            "duration":"61.000000","tags":{"DURATION-eng":"02:21:02.000000000"}}],
            "format":{"duration":"61.000000"}}"#;
        let probe = parse_probe_json(json).unwrap();
        assert!(close(probe.primary_video().unwrap().duration, 61.0));
        assert_eq!(
            probe.primary_video().unwrap().duration_from,
            DurationFrom::Stream
        );
    }

    /// Only the film's tags survived (no plain `DURATION`): they claim far
    /// more than the container holds, so the file's own timing is used.
    #[test]
    fn a_tag_much_longer_than_the_container_is_not_trusted() {
        let probe = parse_probe_json(&mkv_json(STALE, STALE, "61.061000")).unwrap();
        assert!(close(probe.av_duration(), FILM_SECS), "as tagged");
        assert!(probe.has_doubtful_tags());
        assert_eq!(probe.tag_doubt(&probe.streams[0]), Some(TagDoubt::Longer));

        // The last packets could not be read: the container's length.
        let mut without_tail = probe.clone();
        without_tail.settle_tagged_durations(None);
        for stream in &without_tail.streams {
            assert!(close(stream.duration, 61.061), "{stream:?}");
            assert_eq!(stream.duration_from, DurationFrom::Container);
        }
        assert!(!without_tail.has_doubtful_tags());

        // Where the streams' last packets end (the subtitle had none in
        // the last seconds).
        let tail = PacketEnds::parse(
            "stream_index=0|pts_time=60.977000|dts_time=N/A|duration_time=0.042000\n\
             stream_index=1|pts_time=61.020000|dts_time=61.020000|duration_time=0.001000\n\
             stream_index=2|pts_time=N/A|dts_time=61.000000|duration_time=0.032000\n\
             stream_index=1|pts_time=60.000000|dts_time=60.000000|duration_time=0.001000\n",
            "",
        );
        let mut with_tail = probe.clone();
        with_tail.settle_tagged_durations(Some(&tail));
        assert!(close(with_tail.streams[0].duration, 61.019));
        assert!(close(with_tail.streams[1].duration, 61.021));
        assert!(close(with_tail.streams[2].duration, 61.032));
        assert_eq!(with_tail.streams[0].duration_from, DurationFrom::Packets);
        assert!(close(with_tail.streams[3].duration, 61.061));
        assert_eq!(with_tail.streams[3].duration_from, DurationFrom::Container);
        assert!(close(with_tail.av_duration(), 61.032));
    }

    /// A picture or sound tag well short of the container: out of date when
    /// the stream's packets run on to the end, true when the stream really
    /// stops early (a subtitle keeps the container going).
    #[test]
    fn a_tag_much_shorter_than_the_container_is_checked_against_the_packets() {
        let short = r#""DURATION-eng":"00:00:30.000000000""#;
        let probe = parse_probe_json(&mkv_json(short, short, "61.061000")).unwrap();
        assert_eq!(probe.tag_doubt(&probe.streams[0]), Some(TagDoubt::Shorter));
        // A subtitle that ends early is normal.
        assert_eq!(probe.tag_doubt(&probe.streams[3]), None);

        // Packets to the end: the tag is out of date.
        let tail = PacketEnds::parse(
            "stream_index=0|pts_time=61.019000|duration_time=0.042000\n\
             stream_index=1|pts_time=61.000000|duration_time=0.050000\n\
             stream_index=2|pts_time=61.000000|duration_time=0.050000\n",
            "",
        );
        let mut stale = probe.clone();
        stale.settle_tagged_durations(Some(&tail));
        assert!(close(stale.av_duration(), 61.061));
        assert!(
            close(stale.streams[3].duration, 30.0),
            "subtitle left alone"
        );

        // No picture or sound in the last seconds: they really stop at 30 s.
        let tail = PacketEnds::parse("stream_index=3|pts_time=60.000000|duration_time=1.0\n", "");
        let mut early = probe.clone();
        early.settle_tagged_durations(Some(&tail));
        assert!(close(early.av_duration(), 30.0));
        assert_eq!(early.streams[0].duration_from, DurationFrom::LocalizedTag);

        // Nothing to go on: the container's length.
        let mut unknown = probe.clone();
        unknown.settle_tagged_durations(None);
        assert!(close(unknown.av_duration(), 61.061));

        // No container length: nothing to compare with, so the tags aren't
        // taken as they are (see the tests below).
        let mut no_container = probe.clone();
        no_container.duration = None;
        assert!(!no_container.has_doubtful_tags());
        assert_eq!(no_container.length_check(), Some(LengthCheck::Untrusted));
    }

    /// The listing of a file's last packets.
    fn listed(text: &str) -> PacketEnds {
        PacketEnds::parse(text, "")
    }

    /// A Matroska file written as a live stream (`-live 1`) states no
    /// length but the film's `DURATION-eng`. Before, that tag was trusted:
    /// every attempt failed, and the original was called cut short.
    #[test]
    fn a_file_with_no_length_but_stale_tags_is_measured_by_its_packets() {
        let probe = parse_probe_json(&mkv_json_at(STALE, STALE, None, 0.0)).unwrap();
        assert!(probe.matroska);
        assert_eq!(probe.container_duration(), None);
        assert!(!probe.has_doubtful_tags(), "nothing to compare with");
        assert_eq!(probe.length_check(), Some(LengthCheck::Untrusted));
        // Read from the longest it could be: the seek lands on the last
        // keyframe.
        assert!(close(probe.tail_start(), FILM_SECS - 10.0));

        let tail = listed(
            "stream_index=0|pts_time=60.023000|duration_time=0.041000\n\
             stream_index=1|pts_time=61.000000|duration_time=0.021000\n\
             stream_index=2|pts_time=61.008000|duration_time=0.032000\n",
        );
        let mut measured = probe.clone();
        measured.settle_untrusted(Some(&tail));
        assert!(close(measured.container_duration(), 61.04));
        assert!(close(measured.streams[0].duration, 60.064));
        assert!(close(measured.streams[2].duration, 61.04));
        assert_eq!(measured.streams[0].duration_from, DurationFrom::Packets);
        assert!(close(measured.av_duration(), 61.04));
        assert!(close(measured.best_duration(), 61.04));

        // Sound that had no packet at the end and claims 2:21:02: unknown.
        let mut quiet_end = probe.clone();
        quiet_end.settle_untrusted(Some(&listed(
            "stream_index=0|pts_time=60.023000|duration_time=0.041000\n\
             stream_index=1|pts_time=61.000000|duration_time=0.021000\n",
        )));
        assert_eq!(quiet_end.streams[2].duration, None);
        assert_eq!(quiet_end.streams[2].duration_from, DurationFrom::Unknown);
        assert!(close(quiet_end.av_duration(), 61.021));

        // It compares with its conversion as the 1:01 it is.
        let output = parse_probe_json(&mkv_json(FRESH, FRESH, "61.061000")).unwrap();
        let (src, out) = comparable_durations(&ProbeInfo::default(), Some(&measured), &output);
        let check = duration_check(src, out);
        assert_eq!(check.status, CheckStatus::Pass, "{}", check.detail);
        assert_eq!(check.detail, "Matches the original (1:01 vs 1:01)");

        // Its packets couldn't be listed: its length is unknown, never the
        // film's (verification then fails its length check rather than
        // pass unchecked, and the original isn't called cut short; see
        // `an_original_whose_length_cant_be_read_fails_the_length_check`).
        let mut unknown = probe.clone();
        unknown.settle_untrusted(None);
        assert!(unknown.length_unknown);
        assert_eq!(unknown.av_duration(), None);
        assert_eq!(unknown.best_duration(), None);
        let mut nothing = probe.clone();
        nothing.settle_untrusted(Some(&listed("")));
        assert!(nothing.length_unknown);
        assert_eq!(nothing.av_duration(), None);
        assert!(!measured.length_unknown);
    }

    /// Files that state no length at all, in any container: a Matroska file
    /// written as a live stream without tags (`-live 1`, or through a
    /// pipe), a raw elementary stream, a transport stream ffprobe couldn't
    /// estimate. Before, nothing was listed for them, the length check was
    /// skipped and the tester's conversion cut to 30 s replaced a 61 s
    /// original. They are measured by their packets now, all of them (there
    /// is no end to read back from).
    #[test]
    fn a_file_with_no_length_at_all_is_measured_by_its_packets() {
        let elementary = br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264",
            "width":160,"height":90,"avg_frame_rate":"24/1"}],
            "format":{"format_name":"h264"}}"#;
        let transport = br#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"mpeg2video","start_time":"1.400000"},
            {"index":1,"codec_type":"audio","codec_name":"mp2","start_time":"1.400000"},
            {"index":2,"codec_type":"subtitle","codec_name":"dvb_subtitle","duration":"61.0"}],
            "format":{"format_name":"mpegts","start_time":"1.400000"}}"#;
        let live = mkv_json_at("", "", None, 0.0);
        for json in [&live[..], elementary, transport] {
            let probe = parse_probe_json(json).unwrap();
            assert_eq!(probe.av_duration(), None);
            assert_eq!(
                probe.length_check(),
                Some(LengthCheck::Untrusted),
                "{probe:?}"
            );
            assert_eq!(probe.tail_start(), None, "every packet is listed");
        }

        // Listed: as long as its packets, each track as long as its own.
        let mut measured = parse_probe_json(&live).unwrap();
        measured.settle_untrusted(Some(&listed(
            "stream_index=0|pts_time=60.977000|duration_time=0.042000\n\
             stream_index=1|pts_time=61.000000|duration_time=0.022000\n",
        )));
        assert!(close(measured.container_duration(), 61.022));
        assert!(close(measured.streams[0].duration, 61.019));
        assert!(close(measured.av_duration(), 61.022));
        assert!(!measured.length_unknown);
        // A raw stream's packets have no times: their durations add up.
        let mut raw = parse_probe_json(elementary).unwrap();
        let mut listing = String::new();
        for _ in 0..1525 {
            listing.push_str("stream_index=0|pts_time=N/A|dts_time=N/A|duration_time=0.04\n");
        }
        raw.settle_untrusted(Some(&listed(&listing)));
        assert!(close(raw.av_duration(), 61.0), "{raw:?}");

        // A conversion cut to 30 s now fails, a complete one passes.
        let output = |secs: &str| {
            let tags = format!(r#""DURATION":"00:00:{secs}000000""#);
            parse_probe_json(&mkv_json(&tags, "", secs)).unwrap()
        };
        let info = ProbeInfo::default();
        let (src, out) = comparable_durations(&info, Some(&measured), &output("30.000"));
        let check = duration_check(src, out);
        assert_eq!(check.status, CheckStatus::Fail);
        assert_eq!(
            check.detail,
            "The new file is shorter than the original (30.0 s instead of 1:01)"
        );
        let output = parse_probe_json(&mkv_json(FRESH, FRESH, "61.061000")).unwrap();
        let (src, out) = comparable_durations(&info, Some(&measured), &output);
        assert_eq!(duration_check(src, out).status, CheckStatus::Pass);

        // Not listed: unknown (the length check fails).
        let mut unknown = parse_probe_json(&live).unwrap();
        unknown.settle_untrusted(None);
        assert!(unknown.length_unknown);
        assert!(!unknown.cut_off);
        assert_eq!(
            unread_source_length(unknown.cut_off),
            "The original's length couldn't be read (it states none that can be trusted, and \
             its contents couldn't be listed), so the new file couldn't be checked against it"
        );
        // Cut off: it states nothing to keep, so its length is unknown too,
        // never where its packets happen to stop.
        let mut cut = parse_probe_json(&live).unwrap();
        cut.settle_untrusted(Some(&PacketEnds::parse(
            "stream_index=1|pts_time=30.395000|duration_time=0.023000\n",
            "[matroska,webm @ 0x55f274109980] File ended prematurely\n",
        )));
        assert!(cut.length_unknown && cut.cut_off);
        assert_eq!(cut.best_duration(), None);
        assert_eq!(
            unread_source_length(cut.cut_off),
            "The original's length couldn't be read (it stops in the middle of its data and \
             doesn't say how long it should be), so the new file couldn't be checked against it"
        );

        // A file whose picture and sound state their lengths is left alone,
        // as is one whose container does.
        let stated = br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264",
            "duration":"61.000000"}],"format":{"format_name":"mov,mp4,m4a,3gp,3g2,mj2"}}"#;
        assert_eq!(parse_probe_json(stated).unwrap().length_check(), None);
        let ts = br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264"}],
            "format":{"format_name":"mpegts","duration":"61.022667","start_time":"1.400000"}}"#;
        assert_eq!(parse_probe_json(ts).unwrap().length_check(), None);
    }

    /// The same file really cut short: ffprobe says it stops in the middle
    /// of a packet, so where its packets stop is not its length. What it
    /// states stands, and a conversion of what is there is shorter than
    /// that.
    #[test]
    fn a_cut_off_file_keeps_the_length_it_states() {
        let probe = parse_probe_json(&mkv_json_at(STALE, STALE, None, 0.0)).unwrap();
        let cut = PacketEnds::parse(
            "stream_index=0|pts_time=30.398000|duration_time=0.041000\n",
            "[matroska,webm @ 0x55fe3db85900] File ended prematurely\n",
        );
        assert!(cut.cut_off);
        let mut settled = probe.clone();
        settled.settle_untrusted(Some(&cut));
        assert!(close(settled.av_duration(), FILM_SECS));
        let output = parse_probe_json(&mkv_json(
            r#""DURATION":"00:00:30.439000000""#,
            "",
            "30.439000",
        ))
        .unwrap();
        let (src, out) = comparable_durations(&ProbeInfo::default(), Some(&settled), &output);
        assert_eq!(duration_check(src, out).status, CheckStatus::Fail);

        // A tag the container contradicts is not replaced by the packets of
        // a cut-off file either: the container's length stands.
        let short = r#""DURATION-eng":"00:00:30.000000000""#;
        let mut doubted = parse_probe_json(&mkv_json(short, short, "61.061000")).unwrap();
        assert_eq!(doubted.length_check(), Some(LengthCheck::Doubted));
        doubted.settle_tagged_durations(Some(&cut));
        assert!(close(doubted.av_duration(), 61.061));
    }

    /// Timestamps from 10:00 (an ffmpeg cut with `-output_ts_offset`, or
    /// `-copyts`): ffmpeg's writer states where the clip ends, 11:01, as
    /// its length and in every fresh `DURATION`. Before, that was compared
    /// with its conversion's 1:01 and failed, on old and new code alike.
    #[test]
    fn lengths_count_from_where_the_file_starts() {
        let fresh_end = r#""DURATION":"00:11:01.061000000""#;
        let probe = parse_probe_json(&mkv_json_at(
            fresh_end,
            fresh_end,
            Some("661.061000"),
            600.0,
        ))
        .unwrap();
        assert_eq!(probe.start_offset(), 600.0);
        assert!(
            !probe.has_doubtful_tags(),
            "the tags agree with the container"
        );
        assert_eq!(probe.length_check(), Some(LengthCheck::Untrusted));
        // Read as a length from the start (past the end lands on the last
        // keyframe).
        assert!(close(probe.tail_start(), 1251.061));

        let tail = listed(
            "stream_index=0|pts_time=660.023000|duration_time=0.041000\n\
             stream_index=1|pts_time=661.000000|duration_time=0.020000\n\
             stream_index=2|pts_time=661.020000|duration_time=0.032000\n",
        );
        let mut source = probe.clone();
        source.settle_untrusted(Some(&tail));
        assert!(close(source.container_duration(), 61.052));
        assert!(close(source.av_duration(), 61.052));
        // The subtitle ended before the part listed; its tag, read as an
        // end, says so.
        assert!(close(source.streams[3].duration, 61.061));

        let output = parse_probe_json(&mkv_json(FRESH, FRESH, "61.061000")).unwrap();
        assert_eq!(output.length_check(), None, "starts at zero");
        let info = ProbeInfo {
            duration_secs: Some(61.052),
            start_time: Some(600.0),
            ..Default::default()
        };
        let (src, out) = comparable_durations(&info, Some(&source), &output);
        let check = duration_check(src, out);
        assert_eq!(check.status, CheckStatus::Pass, "{}", check.detail);
        assert_eq!(check.detail, "Matches the original (1:01 vs 1:01)");
        assert!(close(expected_play_length(&output, src), 61.061));

        // Its conversion cut short still fails.
        let short = parse_probe_json(&mkv_json(
            r#""DURATION":"00:00:30.000000000""#,
            "",
            "30.000000",
        ))
        .unwrap();
        let (src, out) = comparable_durations(&info, Some(&source), &short);
        let check = duration_check(src, out);
        assert_eq!(check.status, CheckStatus::Fail);
        assert_eq!(
            check.detail,
            "The new file is shorter than the original (30.0 s instead of 1:01)"
        );

        // The packets couldn't be listed: 11:01 might be an end or a
        // length, so it is unknown.
        let mut unknown = probe.clone();
        unknown.settle_untrusted(None);
        assert_eq!(unknown.best_duration(), None);
        assert!(unknown.length_unknown);
        // A cut-off file keeps what it states, read as an end.
        let mut cut = probe.clone();
        cut.settle_untrusted(Some(&PacketEnds {
            cut_off: true,
            ..listed("stream_index=0|pts_time=630.0|duration_time=0.04\n")
        }));
        assert!(close(cut.container_duration(), 61.061));
        assert!(close(cut.av_duration(), 61.061));
    }

    /// Timestamps from 10:00 and only the film's `DURATION-eng`, written
    /// through a pipe: the container states the clip's length, 1:01, the
    /// tag 2:21:02. Before, the last packets were read from 0:51 (the whole
    /// file), their end, 11:01, compared with 1:01, and the clip failed.
    #[test]
    fn a_length_smaller_than_the_start_time_is_a_length() {
        let probe = parse_probe_json(&mkv_json_at(STALE, STALE, Some("61.061000"), 600.0)).unwrap();
        assert_eq!(probe.length_check(), Some(LengthCheck::Untrusted));
        assert!(close(probe.tail_start(), 651.061));
        let tail = listed(
            "stream_index=0|pts_time=660.023000|duration_time=0.041000\n\
             stream_index=1|pts_time=661.040000|duration_time=0.021000\n",
        );
        let mut source = probe.clone();
        source.settle_untrusted(Some(&tail));
        assert!(close(source.av_duration(), 61.061));
        // Stream 2 had no packet listed and claims the film: unknown.
        assert_eq!(source.streams[2].duration, None);
        let output = parse_probe_json(&mkv_json(FRESH, FRESH, "61.061000")).unwrap();
        let (src, out) = comparable_durations(&ProbeInfo::default(), Some(&source), &output);
        assert_eq!(duration_check(src, out).status, CheckStatus::Pass);

        // Not listed: 1:01 is smaller than the start, so it can only be a
        // length; the film's tag is unknown.
        let mut unknown = probe.clone();
        unknown.settle_untrusted(None);
        assert!(close(unknown.best_duration(), 61.061));
        assert_eq!(unknown.av_duration(), None);
        assert!(!unknown.length_unknown, "the container length counts");
    }

    /// The tail read with the real ffprobe: a 12 s video with 8 s of
    /// sound, read from 10 s before the end, finds where each really
    /// stops, and a stale tag gives way to it.
    #[tokio::test]
    async fn reads_where_the_streams_really_end_with_ffprobe() {
        let have = |tool: &str| {
            std::process::Command::new(tool)
                .arg("-version")
                .output()
                .is_ok_and(|o| o.status.success())
        };
        if !have("ffmpeg") || !have("ffprobe") {
            eprintln!("ffmpeg/ffprobe not found; skipping");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tail.mkv");
        let made = tokio::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=160x90:rate=25:duration=12",
            ])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=8"])
            .args(["-c:v", "libx264", "-preset", "ultrafast", "-g", "25"])
            .args(["-c:a", "aac"])
            .arg(&path)
            .output()
            .await
            .unwrap();
        assert!(made.status.success(), "{made:?}");

        let cancel = CancellationToken::new();
        let ffprobe = Path::new("ffprobe");
        let mut probe = probe_media(ffprobe, &path, &cancel).await.unwrap();
        assert!(close(probe.container_duration(), 12.0), "{probe:?}");
        assert!(!probe.has_doubtful_tags(), "ffmpeg's own tags are right");
        assert_eq!(probe.length_check(), None);

        let from = probe.tail_start();
        assert!(close(from, 2.0), "{from:?}");
        let tail = list_packets(ffprobe, &path, from, &cancel).await.unwrap();
        // The seek lands on the keyframe before (one a second).
        let first = tail.first.unwrap_or_default();
        assert!((1.0..=2.1).contains(&first), "{tail:?}");
        assert!(!tail.cut_off);
        let video_end = tail.ends.get(&0).copied().unwrap_or_default();
        let audio_end = tail.ends.get(&1).copied().unwrap_or_default();
        assert!((video_end - 12.0).abs() < 0.1, "{tail:?}");
        assert!((audio_end - 8.0).abs() < 0.1, "{tail:?}");

        // Had the video carried a film's tag, the packets would win.
        probe.streams[0].duration = Some(FILM_SECS);
        probe.streams[0].duration_from = DurationFrom::LocalizedTag;
        assert!(probe.has_doubtful_tags());
        probe.settle_tagged_durations(Some(&tail));
        assert!(close(probe.streams[0].duration, video_end));
        assert_eq!(probe.streams[0].duration_from, DurationFrom::Packets);
        assert!((probe.av_duration().unwrap_or_default() - 12.0).abs() < 0.1);
    }

    /// The owner's job, measured as verification measures it: the clip and
    /// its conversion both carry the film's `DURATION-eng`. Before, the
    /// length check could read "2:21:02 vs 2:21:02" and the full decode
    /// then fail with "Playback stopped at 1:01 of 2:21:02".
    #[test]
    fn stale_tags_on_both_files_compare_the_real_lengths() {
        for (first, second) in [(FRESH, STALE), (STALE, FRESH)] {
            let tags = format!("{first},{second}");
            let source = parse_probe_json(&mkv_json(&tags, &tags, "61.061000")).unwrap();
            let output = parse_probe_json(&mkv_json(&tags, &tags, "61.061000")).unwrap();
            let info = ProbeInfo {
                duration_secs: Some(61.061),
                ..Default::default()
            };
            let (src, out) = comparable_durations(&info, Some(&source), &output);
            assert!(close(src, 61.061) && close(out, 61.061), "{src:?} {out:?}");
            let check = duration_check(src, out);
            assert_eq!(check.status, CheckStatus::Pass);
            assert_eq!(check.detail, "Matches the original (1:01 vs 1:01)");

            let expected = expected_play_length(&output, src);
            assert!(close(expected, 61.061));
            let mut decode = decode_result(&[]);
            decode.decoded_secs = Some(61.04);
            let played = decode_check(&decode, expected, &Inherited::default());
            assert_eq!(played.status, CheckStatus::Pass, "{}", played.detail);
            assert_eq!(played.detail, "Decoded all 1:01 without errors");
        }
    }

    /// A truncated conversion still fails, stale tags or not: the length
    /// check when the new file says it is shorter, the full decode when it
    /// claims the whole length but stops early.
    #[test]
    fn truncated_output_still_fails_with_stale_tags() {
        let tags = format!("{FRESH},{STALE}");
        let source = parse_probe_json(&mkv_json(&tags, &tags, "61.061000")).unwrap();
        let info = ProbeInfo {
            duration_secs: Some(61.061),
            ..Default::default()
        };

        // Cut short at 30 s, the film's tag still on every track.
        let short = format!(r#""DURATION":"00:00:30.000000000",{STALE}"#);
        let output = parse_probe_json(&mkv_json(&short, &short, "30.000000")).unwrap();
        let (src, out) = comparable_durations(&info, Some(&source), &output);
        let check = duration_check(src, out);
        assert_eq!(check.status, CheckStatus::Fail);
        assert_eq!(
            check.detail,
            "The new file is shorter than the original (30.0 s instead of 1:01)"
        );

        // Only the film's tag left on the cut file: it can't pass for the
        // original's length either.
        let mut output = parse_probe_json(&mkv_json(STALE, STALE, "30.000000")).unwrap();
        output.settle_tagged_durations(None);
        let (src, out) = comparable_durations(&info, Some(&source), &output);
        assert!(close(out, 30.0), "{out:?}");
        assert_eq!(duration_check(src, out).status, CheckStatus::Fail);

        // The header claims the whole length (stale tag included) but the
        // pictures stop at 30 s.
        let output = parse_probe_json(&mkv_json(&tags, &tags, "61.061000")).unwrap();
        let (src, out) = comparable_durations(&info, Some(&source), &output);
        assert_eq!(duration_check(src, out).status, CheckStatus::Pass);
        let expected = expected_play_length(&output, src);
        assert!(close(expected, 61.061), "never the film's 2:21:02");
        let mut decode = decode_result(&[]);
        decode.decoded_secs = Some(30.0);
        let played = decode_check(&decode, expected, &Inherited::default());
        assert_eq!(played.status, CheckStatus::Fail);
        assert_eq!(played.detail, "Playback stopped at 30.0 s of 1:01");

        // Without the container's word, a stale tag can't stretch what a
        // decode has to reach past the file's real end either.
        let mut stale_only = parse_probe_json(&mkv_json(STALE, STALE, "61.061000")).unwrap();
        stale_only.settle_tagged_durations(None);
        assert!(close(expected_play_length(&stale_only, src), 61.061));
    }

    #[test]
    fn input_windows_seek_early_and_read_past_the_end() {
        // MPEG-TS style: video starts 0.011 s after the container.
        let args = input_window(1.483, 1.472, 10.0, 12.0, Some(5.0));
        assert_eq!(args, ["-ss", "5.011000", "-t", "8.000000"]);
        // Near the start there is nothing to skip.
        assert_eq!(
            input_window(0.0, 0.0, 2.0, 4.0, Some(5.0)),
            ["-t", "5.000000"]
        );
        // No preroll: read from the start.
        assert_eq!(
            input_window(0.0, -0.023, 10.0, 12.0, None),
            ["-t", "13.023000"]
        );
    }

    fn request<'a>(probe: &'a ProbeInfo, profile: &'a TranscodeProfile) -> ValidateRequest<'a> {
        ValidateRequest {
            ffmpeg: Path::new("ffmpeg"),
            ffprobe: Path::new("ffprobe"),
            source: Path::new("/m/in.ts"),
            source_probe: probe,
            output: Path::new("/t/out.mkv"),
            profile,
            level: ValidationLevel::Standard,
            expected: StreamSummary::default(),
        }
    }

    #[test]
    fn segment_commands_search_offsets_at_low_resolution() {
        let probe = ProbeInfo::default();
        let profile = TranscodeProfile::default();
        let req = request(&probe, &profile);
        let align = Alignment {
            source_video_start: 1.5,
            source_format_start: 1.4,
            output_format_start: -0.023,
            timeline_shift: 0.1,
            width: 720,
            height: 576,
            scale_source: true,
            compare_size: (536, 428),
            rate_expr: "25/1".into(),
            rate: 25.0,
            source_fps: 25.0,
            deinterlace: Some("send_frame"),
            ..test_alignment()
        };
        let window = SegmentWindow {
            at: 10.0,
            len: 2.0,
            shift: 0.1,
            search_before: 3,
            search_after: 3,
            source_preroll: Some(SEEK_PREROLL_SECS),
        };
        let paths = stats_paths(Path::new("/tmp"), 0, window.offsets());
        assert_eq!(paths.len(), 7);
        let args = segment_args(&req, &align, &window, &paths);
        assert!(args.contains(&"-copyts".to_string()));
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        // Reference: 3 frames (0.12 s) either side of the segment.
        assert!(
            graph.contains("[0:0]bwdif=mode=send_frame,trim=start=11.380000:end=13.640000"),
            "{graph}"
        );
        assert!(graph.contains("scale=720:576:flags=bicubic,scale=536:428:flags=area"));
        // Output: the same pictures 0.1 s later on its timeline.
        assert!(graph.contains("[1:0]trim=start=10.100000:end=12.100000,setpts=PTS-STARTPTS"));
        assert!(graph.contains("[r0]trim=start_frame=0,"));
        assert!(graph.contains("[r6]trim=start_frame=6,"));
        assert!(graph.contains("ssim=stats_file=/tmp/seg0_-3_ssim.log:shortest=1[vs0]"));
        assert!(graph.contains("psnr=stats_file=/tmp/seg0_3_psnr.log:shortest=1[vp6]"));
        assert_eq!(args.iter().filter(|a| *a == "-map").count(), 14);
    }

    #[test]
    fn scan_commands_align_and_detect_on_both_sides() {
        let probe = ProbeInfo::default();
        let profile = TranscodeProfile::default();
        let req = request(&probe, &profile);
        let align = Alignment {
            source_video_start: 2.1,
            source_format_start: 1.4,
            ..test_alignment()
        };
        let paths: Vec<PathBuf> = (0..3)
            .map(|k| PathBuf::from(format!("/tmp/scan{k}.log")))
            .collect();
        // Segments matched 0.7 s later, one frame further on.
        let plan = ScanPlan::new(
            &align,
            SegmentAlignment {
                shift: 0.7,
                frames: 1,
            },
        );
        assert_eq!((plan.output_skip, plan.reference_skip), (0, 1));
        let args = scan_args(&req, &align, &plan, &paths);
        assert!(args.contains(&"-copyts".to_string()));
        let graph = &args[args.iter().position(|a| a == "-filter_complex").unwrap() + 1];
        // Both cut a quarter frame before the first source frame; the
        // output 0.7 s later, past the frames padded in front of it.
        assert!(graph.contains("[0:0]trim=start=2.089583,"), "{graph}");
        assert!(graph.contains("[1:0]trim=start=0.689583,"), "{graph}");
        assert!(graph.contains("scale=320:180:flags=area"));
        assert!(graph.contains(
            "blackdetect@src=d=0.5:pix_th=0.10,gblur=sigma=1,freezedetect@src=n=-45dB:d=0.5,nullsink"
        ));
        assert!(graph.contains("freezedetect@out="));
        assert!(graph.contains("[s0]trim=start_frame=0,"));
        assert!(graph.contains("[s2]trim=start_frame=2,"));
        assert!(graph.contains("[o2]trim=start_frame=0,"));
        assert!(graph.contains("ssim=stats_file=/tmp/scan2.log:shortest=1[v2]"));

        // Matched a frame earlier: the output skips a frame instead.
        let plan = ScanPlan::new(
            &align,
            SegmentAlignment {
                shift: 0.0,
                frames: -2,
            },
        );
        assert_eq!((plan.output_skip, plan.reference_skip), (3, 1));
        assert_eq!(
            common_alignment(&[
                SegmentAlignment {
                    shift: 0.7,
                    frames: 1
                },
                SegmentAlignment {
                    shift: 0.0,
                    frames: 0
                },
                SegmentAlignment {
                    shift: 0.7,
                    frames: 2
                },
                SegmentAlignment {
                    shift: 0.7,
                    frames: 1
                },
            ]),
            Some(SegmentAlignment {
                shift: 0.7,
                frames: 1
            })
        );
    }

    #[test]
    fn alignment_follows_the_shared_timeline() {
        let probe = |json: &[u8]| parse_probe_json(json).unwrap();
        // Broadcast recording: audio from 36001.389, video from 36002.080.
        let source = probe(
            br#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"mpeg2video","width":1280,"height":720,
             "avg_frame_rate":"25/1","start_time":"36002.080000"},
            {"index":1,"codec_type":"audio","codec_name":"mp2","start_time":"36001.389089"}],
            "format":{"start_time":"36001.389089"}}"#,
        );
        // MP4 output: video padded to start with the audio at 0.
        let output = probe(
            br#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","width":1280,"height":720,
             "avg_frame_rate":"25/1","start_time":"0.000000"},
            {"index":1,"codec_type":"audio","codec_name":"aac","start_time":"0.000000"}],
            "format":{"start_time":"0.000000"}}"#,
        );
        let align = Alignment::new(&ProbeInfo::default(), Some(&source), &output).unwrap();
        assert!((align.timeline_shift - 0.690911).abs() < 1e-6);
        assert!((align.av_timeline_shift - 0.690911).abs() < 1e-6);
        assert!(!align.scale_source);
        // Far from the per-file guess, so both are searched.
        assert_eq!(align.shift_candidates().len(), 2);

        // A subtitle track that starts early does not move the picture.
        let with_subs = probe(
            br#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","width":1280,"height":720,
             "avg_frame_rate":"25/1","start_time":"10.000000"},
            {"index":1,"codec_type":"audio","codec_name":"aac","start_time":"10.000000"},
            {"index":2,"codec_type":"subtitle","codec_name":"subrip","start_time":"0.000000"}],
            "format":{"start_time":"0.000000"}}"#,
        );
        let align = Alignment::new(&ProbeInfo::default(), Some(&with_subs), &output).unwrap();
        assert_eq!(align.av_timeline_shift, 0.0);
        assert_eq!(align.timeline_shift, 10.0);
        assert_eq!(align.shift_candidates(), [0.0, 10.0]);

        // Matroska keeps the timestamps: no shift.
        let kept = probe(
            br#"{"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","width":1280,"height":720,
             "avg_frame_rate":"25/1","start_time":"0.691000"},
            {"index":1,"codec_type":"audio","codec_name":"aac","start_time":"0.000000"}],
            "format":{"start_time":"0.000000"}}"#,
        );
        let align = Alignment::new(&ProbeInfo::default(), Some(&source), &kept).unwrap();
        assert!(align.timeline_shift.abs() < 1e-3);
        assert_eq!(align.shift_candidates(), [align.timeline_shift]);
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
        // The check line reads like the rest of the job sheet.
        assert_eq!(human_bytes(3_130_000), "3.13 MB");
        assert_eq!(human_bytes(572_400), "572 KB");
        assert_eq!(human_bytes(999_999_950), "1 GB");
    }

    #[test]
    fn stats_paths_are_per_segment_and_offset() {
        let paths = stats_paths(Path::new("/tmp/x"), 3, -1..=1);
        assert_eq!(paths.len(), 3);
        assert_eq!(paths[0].0, -1);
        assert_eq!(paths[0].1, Path::new("/tmp/x/seg3_-1_ssim.log"));
        assert_eq!(paths[2].2, Path::new("/tmp/x/seg3_1_psnr.log"));
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

    /// An original that states no length but the film's tag and whose
    /// packets can't be listed: its length is unknown, never the film's.
    /// With nothing to tell a complete new file from one cut short, the
    /// length check fails (the original is kept) rather than pass
    /// unchecked; the original isn't called cut short either (see
    /// `source_length`).
    #[cfg(unix)]
    #[tokio::test]
    async fn an_original_whose_length_cant_be_read_fails_the_length_check() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let source_json = String::from_utf8(mkv_json_at(STALE, STALE, None, 0.0)).unwrap();
        let output_json = String::from_utf8(mkv_json(FRESH, FRESH, "61.061000")).unwrap();
        let script = dir.path().join("ffprobe");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$*\" in\n*-show_entries*) exit 1 ;;\n\
                 *live.mkv) echo '{}' ;;\n*) echo '{}' ;;\nesac\n",
                source_json.replace('\n', " "),
                output_json.replace('\n', " ")
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (source, output) = (dir.path().join("live.mkv"), dir.path().join("out.mkv"));
        let cancel = CancellationToken::new();

        // ("text file busy" while another test's fork holds the script.)
        let mut probed = None;
        for _ in 0..50 {
            match probe_media(&script, &source, &cancel).await {
                Err(ProbeError::Failed(why)) if why.contains("busy") => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => {
                    probed = Some(other);
                    break;
                }
            }
        }
        let probe = match probed {
            Some(Ok(probe)) => probe,
            other => panic!("{other:?}"),
        };
        assert!(probe.length_unknown);
        assert_eq!(probe.best_duration(), None);
        assert_eq!(
            source_length(&script, &source, &cancel).await.ok(),
            Some(SourceLength {
                length: None,
                packets: PacketsEnd::Unlisted
            })
        );

        let info = ProbeInfo {
            duration_secs: Some(FILM_SECS),
            ..Default::default()
        };
        let profile = TranscodeProfile {
            video_codec: VideoCodec::Hevc,
            ..TranscodeProfile::default()
        };
        let req = ValidateRequest {
            ffmpeg: Path::new("ffmpeg"),
            ffprobe: &script,
            source: &source,
            source_probe: &info,
            output: &output,
            profile: &profile,
            level: ValidationLevel::Standard,
            expected: StreamSummary {
                video: 1,
                audio: 2,
                subtitle: 1,
            },
        };
        let report = validate_output(&req, &cancel, &|_| {}).await;
        assert!(!report.passed);
        let length = report.checks.iter().find(|c| c.id == "duration").unwrap();
        assert_eq!(length.status, CheckStatus::Fail, "{report:?}");
        assert!(
            length
                .detail
                .starts_with("The original's length couldn't be read"),
            "{}",
            length.detail
        );
    }
}
