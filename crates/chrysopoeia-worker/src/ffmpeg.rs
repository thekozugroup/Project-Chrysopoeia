//! Spawning ffmpeg, parsing `-progress` output, capturing stderr.
//!
//! Every ffmpeg (and verification) process the worker starts goes through
//! [`run_ffmpeg`]: stdin is closed, the child is killed if the future is
//! dropped, stdout is parsed as `-progress pipe:1` key/value blocks, and the
//! last [`STDERR_TAIL_LINES`] stderr lines are kept for error reports.
//! Cancellation kills ffmpeg at once: a cancelled encode is thrown away, so
//! there is nothing worth waiting for (x265 and SVT-AV1 ignore SIGTERM for
//! many seconds while they flush their frame queues). A process that prints
//! nothing for its stall timeout is treated as hung and killed the same way.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use chrysopoeia_core::ProgressBasis;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader, Split};
use tokio::process::{Child, Command};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Number of stderr lines kept for error reports.
pub const STDERR_TAIL_LINES: usize = 40;

/// Default time without progress after which an encode counts as hung.
/// ffmpeg's progress blocks move on every half second while it works, so
/// ten minutes without a frame, a moment of output or a byte written only
/// happen when it is stuck (for example in a GPU driver). ffmpeg 7 keeps
/// printing progress blocks while it is stuck, so blocks that repeat the
/// last one don't count as progress (see [`Liveness`]).
pub const DEFAULT_STALL_TIMEOUT: Duration = Duration::from_secs(600);

/// Tells ffmpeg working apart from ffmpeg merely printing. ffmpeg 6 prints a
/// progress block only when something happened, but ffmpeg 7 (the Docker
/// image's jellyfin-ffmpeg) prints one every half second from its main
/// thread even when no frame moves, so a hung encoder or a stuck read would
/// never look silent.
#[derive(Debug, Default)]
struct Liveness {
    /// Frame count, output time and bytes written of the last block.
    last: Option<(Option<u64>, Option<u64>, Option<u64>)>,
    /// The last stderr line.
    last_stderr: Option<String>,
}

impl Liveness {
    /// Whether a progress block shows work done since the previous block
    /// (the first block and the final one always do).
    fn block(&mut self, block: &ProgressBlock) -> bool {
        let marks = (
            block.frame,
            block.out_time_secs.map(f64::to_bits),
            block.total_size,
        );
        let moved = block.end || self.last != Some(marks);
        self.last = Some(marks);
        moved
    }

    /// Whether a stderr line is news: a line repeated over and over (a
    /// decoder complaining in a loop) is not progress by itself.
    fn stderr(&mut self, line: &str) -> bool {
        if self.last_stderr.as_deref() == Some(line) {
            return false;
        }
        self.last_stderr = Some(line.to_string());
        true
    }
}

/// Niceness used for low-priority encodes.
const NICE_LEVEL: &str = "10";

/// One `-progress` report. ffmpeg prints a block of `key=value` lines ending
/// with `progress=continue` (or `progress=end` for the last one).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProgressBlock {
    /// Frames written so far.
    pub frame: Option<u64>,
    /// Position in the output, in seconds.
    pub out_time_secs: Option<f64>,
    /// Encoding frames per second.
    pub fps: Option<f32>,
    /// Realtime multiple (2.31 for `speed=2.31x`).
    pub speed: Option<f32>,
    /// Bytes written so far.
    pub total_size: Option<u64>,
    /// True for the final block (`progress=end`).
    pub end: bool,
}

/// Incremental parser for `-progress` output.
#[derive(Debug, Default)]
pub struct ProgressParser {
    current: ProgressBlock,
}

impl ProgressParser {
    /// An empty parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one line. Returns the finished block when the line is the
    /// `progress=` terminator.
    pub fn push_line(&mut self, line: &str) -> Option<ProgressBlock> {
        let (key, value) = line.trim().split_once('=')?;
        let value = value.trim();
        match key.trim() {
            "frame" => self.current.frame = value.parse().ok(),
            // Despite its name, `out_time_ms` is also in microseconds.
            "out_time_us" | "out_time_ms" => {
                if let Some(secs) = parse_out_time_us(value) {
                    self.current.out_time_secs = Some(secs);
                }
            }
            "out_time" => {
                if self.current.out_time_secs.is_none() {
                    self.current.out_time_secs = parse_out_time_clock(value);
                }
            }
            "fps" => self.current.fps = value.parse::<f32>().ok().filter(|f| f.is_finite()),
            "speed" => self.current.speed = parse_speed(value),
            "total_size" => self.current.total_size = value.parse().ok(),
            "progress" => {
                let mut block = std::mem::take(&mut self.current);
                block.end = value == "end";
                return Some(block);
            }
            _ => {}
        }
        None
    }
}

/// Parse an `out_time_us`/`out_time_ms` value (microseconds) into seconds.
/// `N/A` gives `None`; negative values (before the first frame) give 0.
/// ffmpeg before 6 printed "no time yet" as the lowest number it has
/// (`-9223372036854775807`); that, like any time more than a day before the
/// start, gives `None` too.
pub fn parse_out_time_us(value: &str) -> Option<f64> {
    let micros: i64 = value.trim().parse().ok()?;
    if micros < -NO_TIME_YET_SECS * 1_000_000 {
        return None;
    }
    Some(micros.max(0) as f64 / 1_000_000.0)
}

/// An output time further before the start than this is ffmpeg's "no time
/// yet" (see [`parse_out_time_us`]).
const NO_TIME_YET_SECS: i64 = 86_400;

/// Parse the `out_time` field (`HH:MM:SS.fraction`, see [`parse_clock`]),
/// with ffmpeg's old "no time yet" (`-2562047:47:16.854775`) as `None`.
fn parse_out_time_clock(value: &str) -> Option<f64> {
    match value.trim().strip_prefix('-') {
        Some(magnitude) => {
            let secs = parse_clock(magnitude)?;
            (secs <= NO_TIME_YET_SECS as f64).then_some(0.0)
        }
        None => parse_clock(value),
    }
}

/// Parse an ffmpeg speed value such as `2.31x` or ` 0.5x`. `N/A` and
/// non-positive values give `None`.
pub fn parse_speed(value: &str) -> Option<f32> {
    let number = value.trim().trim_end_matches('x').trim();
    number
        .parse::<f32>()
        .ok()
        .filter(|s| s.is_finite() && *s > 0.0)
}

/// Parse `HH:MM:SS.fraction` (the `out_time` field, Matroska `DURATION`
/// tags) into seconds. Negative values give 0.
pub(crate) fn parse_clock(value: &str) -> Option<f64> {
    let value = value.trim();
    let negative = value.starts_with('-');
    let mut parts = value.trim_start_matches('-').split(':');
    let hours: f64 = parts.next()?.parse().ok()?;
    let minutes: f64 = parts.next()?.parse().ok()?;
    let seconds: f64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let total = hours * 3600.0 + minutes * 60.0 + seconds;
    Some(if negative { 0.0 } else { total })
}

/// Progress of an encode, derived from a [`ProgressBlock`] and the source
/// duration.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Progress {
    /// 0..=100. Means nothing when `basis` is [`ProgressBasis::Unknown`]
    /// (it is 0 then).
    pub percent: f32,
    /// Encoding frames per second.
    pub fps: Option<f32>,
    /// Realtime multiple.
    pub speed: Option<f32>,
    /// Estimated seconds left. `None` whenever `percent` is unknown.
    pub eta_secs: Option<u64>,
    /// Video frames encoded so far, when ffmpeg says.
    pub frames: Option<u64>,
    /// How `percent` was worked out.
    pub basis: ProgressBasis,
}

/// What an encode's progress is measured against.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Timeline {
    /// The source's length in seconds.
    pub duration_secs: Option<f64>,
    /// How many video frames the encode should give (the source's length
    /// times its frame rate), when that can be trusted.
    pub expected_frames: Option<f64>,
}

/// How far ahead (in percentage points) the frame count must be of the
/// output time before the frames are taken as the better measure. ffmpeg 7
/// reports the output time of the track that is furthest behind, so a
/// subtitle track whose next line is minutes away holds it back while the
/// video moves on.
const FRAMES_LEAD_POINTS: f64 = 2.0;

/// The highest share an estimate from frames shows before ffmpeg says it
/// has finished: frames can't tell that the end has been reached.
const FRAMES_PERCENT_CAP: f64 = 99.0;

/// More frames than the source should have, by this share (plus a couple
/// of frames), means the expected count was wrong: the estimate is dropped.
const FRAMES_OVERSHOOT: f64 = 0.05;

/// Turn a progress block into percent/ETA.
///
/// `duration_secs` is the source duration (an unknown duration gives an
/// unknown percentage until the final block). `elapsed_secs` is the
/// wall-clock time since the encode started; it estimates the ETA when
/// ffmpeg reports no speed. This knows no frame count to estimate from; see
/// [`estimate_progress`].
pub fn compute_progress(
    block: &ProgressBlock,
    duration_secs: Option<f64>,
    elapsed_secs: f64,
) -> Progress {
    estimate_progress(
        block,
        Timeline {
            duration_secs,
            expected_frames: None,
        },
        elapsed_secs,
    )
}

/// Turn a progress block into percent/ETA, from ffmpeg's output time over
/// the source's length, or, when ffmpeg doesn't report an output time yet
/// (or one that is behind the video), from the frames encoded over the
/// frames the source should have ([`Timeline::expected_frames`], an
/// estimate: never 100 % before the final block). With neither, the
/// percentage is [`ProgressBasis::Unknown`]: 0 and no ETA, never a figure
/// that only looks like one.
pub fn estimate_progress(block: &ProgressBlock, timeline: Timeline, elapsed_secs: f64) -> Progress {
    let duration = timeline.duration_secs.filter(|d| d.is_finite() && *d > 0.0);
    let base = Progress {
        percent: 0.0,
        fps: block.fps,
        speed: block.speed,
        eta_secs: None,
        frames: block.frame,
        basis: ProgressBasis::Unknown,
    };
    if block.end {
        return Progress {
            percent: 100.0,
            eta_secs: Some(0),
            basis: ProgressBasis::Time,
            ..base
        };
    }
    let done = block.out_time_secs.map(|t| t.max(0.0));
    let by_time = done
        .zip(duration)
        .map(|(done, d)| ((done / d) * 100.0).clamp(0.0, 100.0));
    let by_frames = frames_percent(block.frame, timeline.expected_frames);
    match (by_time, by_frames) {
        (Some(t), Some(f)) if f >= t + FRAMES_LEAD_POINTS => {
            frames_progress(base, f, timeline, elapsed_secs)
        }
        (Some(t), _) => {
            let (done, d) = (done.unwrap_or(0.0), duration.unwrap_or(0.0));
            let rate = match block.speed {
                Some(speed) if speed > 0.01 => Some(f64::from(speed)),
                _ if elapsed_secs > 1.0 && done > 0.0 => Some(done / elapsed_secs),
                _ => None,
            };
            Progress {
                percent: t as f32,
                eta_secs: rate.and_then(|rate| eta((d - done).max(0.0) / rate)),
                basis: ProgressBasis::Time,
                ..base
            }
        }
        (None, Some(f)) => frames_progress(base, f, timeline, elapsed_secs),
        (None, None) => base,
    }
}

/// The share of the source `frames` stand for, when the expected count is
/// known and the frames haven't outrun it (then it was wrong).
fn frames_percent(frames: Option<u64>, expected: Option<f64>) -> Option<f64> {
    let expected = expected.filter(|e| e.is_finite() && *e >= 1.0)?;
    let frames = frames? as f64;
    if frames > expected * (1.0 + FRAMES_OVERSHOOT) + 2.0 {
        return None;
    }
    Some((frames / expected * 100.0).clamp(0.0, FRAMES_PERCENT_CAP))
}

/// Progress estimated from frames: `percent` of the source, the time left
/// from the frames still to encode at the pace so far.
fn frames_progress(
    base: Progress,
    percent: f64,
    timeline: Timeline,
    elapsed_secs: f64,
) -> Progress {
    let frames = base.frames.unwrap_or(0) as f64;
    // No pace before the first frame.
    let rate = match base.fps {
        _ if frames < 1.0 => None,
        Some(fps) if fps > 0.01 => Some(f64::from(fps)),
        _ if elapsed_secs > 1.0 => Some(frames / elapsed_secs),
        _ => None,
    };
    let left = timeline
        .expected_frames
        .map(|expected| (expected - frames).max(0.0));
    Progress {
        percent: percent as f32,
        eta_secs: rate.zip(left).and_then(|(rate, left)| eta(left / rate)),
        basis: ProgressBasis::Frames,
        ..base
    }
}

/// Whole seconds left, when the estimate is a usable number.
fn eta(secs: f64) -> Option<u64> {
    (secs.is_finite() && secs >= 0.0).then(|| secs.round() as u64)
}

/// Ring buffer of the most recent stderr lines.
#[derive(Debug, Clone)]
pub struct StderrTail {
    lines: VecDeque<String>,
    capacity: usize,
}

impl Default for StderrTail {
    fn default() -> Self {
        Self::new(STDERR_TAIL_LINES)
    }
}

impl StderrTail {
    /// A buffer keeping at most `capacity` lines.
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(capacity.min(64)),
            capacity: capacity.max(1),
        }
    }

    /// Add a line, dropping the oldest one when full. Empty lines and
    /// harmless noise (see [`is_harmless_noise`]) are ignored.
    pub fn push(&mut self, line: &str) {
        let line = line.trim_end();
        if line.trim().is_empty() || is_harmless_noise(line) {
            return;
        }
        if self.lines.len() == self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line.to_string());
    }

    /// The kept lines, oldest first.
    pub fn lines(&self) -> impl DoubleEndedIterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    /// The kept lines joined with newlines.
    pub fn joined(&self) -> String {
        self.lines().collect::<Vec<_>>().join("\n")
    }

    /// The most useful line for an error message (see [`failure_reason`]).
    pub fn last_meaningful(&self) -> Option<String> {
        failure_reason(self.lines())
    }
}

/// Lines some encoder libraries print straight to stderr that mean nothing
/// for the result. libnuma (used by libx265) reports every NUMA memory call
/// Docker's default seccomp profile refuses (`set_mempolicy: Operation not
/// permitted`, many times per encode); x265 simply runs without NUMA
/// placement, which makes no difference on a single-socket machine.
const HARMLESS_NOISE: &[&str] = &[
    "set_mempolicy: Operation not permitted",
    "get_mempolicy: Operation not permitted",
    "mbind: Operation not permitted",
    "migrate_pages: Operation not permitted",
];

/// Whether a stderr line is harmless noise (see [`HARMLESS_NOISE`]): kept out
/// of log tails and never taken for a failure reason.
pub fn is_harmless_noise(line: &str) -> bool {
    HARMLESS_NOISE.iter().any(|n| line.contains(n))
}

/// Messages ffmpeg prints as a consequence of an earlier error. They name
/// no cause, so they are used only when nothing better is available.
const CONSEQUENCES: &[&str] = &[
    "Conversion failed!",
    "Exiting normally, received signal",
    "Nothing was written into output file",
    "Error while filtering",
    "Error sending frames to consumers",
    "Error muxing a packet",
    "Error closing file",
    "Task finished with error code",
    "Terminating thread with return code",
    "Could not open encoder before EOF",
    "Error while processing the decoded data",
];

/// Statistics encoders print when they close, even after a failure (the AAC
/// encoder's `Qavg`, libx264's frame-type summary, the final `frame=` line).
const STATISTICS: &[&str] = &[
    "Qavg:",
    "kb/s:",
    "frame I:",
    "frame P:",
    "frame B:",
    "mb I ",
    "mb P ",
    "mb B ",
    "Avg QP",
    "coded y,",
    "i16 v,h,dc,p",
    "i8 v,h,dc",
    "i4 v,h,dc",
    "i8c dc,h,v,p",
    "ref P L0",
    "ref B L0",
    "ref B L1",
    "Weighted P-Frames",
    "consecutive B-frames",
    "8x8 transform",
    "direct mvs",
    "frame=",
    "video:",
    "encoded ",
    "x265 [info]",
    "Svt[info]",
];

/// Decoder messages about damaged input. A failed encode of a damaged
/// source usually has a more specific cause, so these rank below other
/// errors.
const DAMAGE: &[&str] = &[
    "error while decoding",
    "concealing",
    "corrupt",
    "decode_slice_header error",
    "Invalid NAL unit",
    "missing picture",
    "no frame!",
    "Error submitting packet to decoder",
];

/// Demuxer messages about an input that is cut off or has broken packets.
const TRUNCATION: &[&str] = &[
    "file ended prematurely",
    "truncating packet",
    "packet corrupt",
    "partial file",
    "unexpected end of file",
    "premature end",
    "invalid data found when processing input",
];

/// Whether a stderr line (at warning level or above, or untagged) reports a
/// damaged or cut-off input: a demuxer finding the file ends early, or a
/// decoder patching up broken frames.
pub fn is_input_damage(line: &str) -> bool {
    let (_, level, message) = split_log_line(line);
    if matches!(level, Some("info" | "verbose" | "debug" | "trace")) || is_harmless_noise(line) {
        return false;
    }
    let lower = message.to_ascii_lowercase();
    TRUNCATION.iter().any(|n| lower.contains(n))
        || DAMAGE
            .iter()
            .any(|n| lower.contains(&n.to_ascii_lowercase()))
}

/// Words that mark an untagged line (ffmpeg run without `level+`) as an
/// error rather than a notice.
const ERROR_WORDS: &[&str] = &[
    "error",
    "cannot",
    "could not",
    "failed",
    "invalid",
    "unknown",
    "unrecognized",
    "unsupported",
    "not found",
    "no such",
    "denied",
    "no space",
    "out of memory",
    "not permitted",
    "read-only",
];

/// The most useful line of a failed run's stderr for an error message,
/// without its `[codec @ 0x…]`/`[level]` prefixes.
///
/// ffmpeg reports the cause first and its consequences after it, and
/// encoders print statistics even when the run failed. So: the first
/// error-level line that names a cause, then the first decoder-damage or
/// consequence line, then (for untagged output) the first line that reads
/// like an error, and only then the last line that is not statistics.
pub fn failure_reason<'a>(lines: impl Iterator<Item = &'a str>) -> Option<String> {
    let parsed: Vec<(Option<&str>, &str)> = lines
        .filter(|l| !is_harmless_noise(l))
        .map(|l| {
            let (_, level, message) = split_log_line(l);
            (level, message.trim())
        })
        .filter(|(_, m)| !m.is_empty())
        .collect();
    let starts = |m: &str, list: &[&str]| list.iter().any(|p| m.starts_with(p));
    let contains = |m: &str, list: &[&str]| {
        let lower = m.to_ascii_lowercase();
        list.iter().any(|p| lower.contains(&p.to_ascii_lowercase()))
    };
    let is_error = |level: Option<&str>| matches!(level, Some("error" | "fatal" | "panic"));
    let tagged = parsed.iter().any(|(level, _)| level.is_some());

    let errors: Vec<&str> = parsed
        .iter()
        .filter(|(level, m)| {
            if tagged {
                is_error(*level)
            } else {
                contains(m, ERROR_WORDS) && !starts(m, STATISTICS)
            }
        })
        .map(|(_, m)| *m)
        .collect();
    let pick = errors
        .iter()
        .find(|m| !starts(m, CONSEQUENCES) && !contains(m, DAMAGE))
        .or_else(|| errors.iter().find(|m| !starts(m, CONSEQUENCES)))
        .or_else(|| errors.first());
    if let Some(m) = pick {
        return Some((*m).to_string());
    }
    parsed
        .iter()
        .rev()
        .map(|(_, m)| *m)
        .find(|m| !starts(m, STATISTICS) && !starts(m, CONSEQUENCES))
        .map(str::to_string)
}

/// A plain-language explanation for well-known failure messages, e.g. a
/// full disk or a missing GPU driver.
pub fn explain_failure(message: &str) -> Option<&'static str> {
    const HINTS: &[(&str, &str)] = &[
        (
            "no space left on device",
            "The disk ran out of space while writing the new file",
        ),
        (
            "disk quota exceeded",
            "The disk quota ran out while writing the new file",
        ),
        (
            "read-only file system",
            "The folder the new file is written to is read-only",
        ),
        (
            "cannot load libcuda",
            "The NVIDIA driver could not be loaded. Check that the GPU is passed through to the container",
        ),
        (
            "cannot load libnvidia-encode",
            "The NVIDIA encoder library could not be loaded. Check that the GPU is passed through to the container",
        ),
        (
            "failed to initialise vaapi connection",
            "The Intel/AMD GPU could not be opened. Check that /dev/dri is passed through to the container",
        ),
        (
            "no va display found",
            "The Intel/AMD GPU could not be opened. Check that /dev/dri is passed through to the container",
        ),
        ("cannot allocate memory", "The system ran out of memory"),
        ("out of memory", "The system ran out of memory"),
    ];
    let lower = message.to_ascii_lowercase();
    HINTS
        .iter()
        .find(|(needle, _)| lower.contains(needle))
        .map(|(_, hint)| *hint)
}

/// Start a program with `spawn`, waiting and trying again (after each of
/// `delays`) while too many files are open: a busy moment shouldn't fail a
/// job for good. `Ok(None)` when cancelled while waiting.
pub(crate) async fn spawn_patiently<T>(
    mut spawn: impl FnMut() -> std::io::Result<T>,
    delays: &[Duration],
    cancel: &CancellationToken,
) -> std::io::Result<Option<T>> {
    let mut delays = delays.iter();
    loop {
        match spawn() {
            Ok(child) => return Ok(Some(child)),
            Err(e) if chrysopoeia_core::process::out_of_file_handles(&e) => {
                let Some(delay) = delays.next() else {
                    return Err(e);
                };
                tracing::debug!("too many files are open to start a program; waiting {delay:?}");
                tokio::select! {
                    () = cancel.cancelled() => return Ok(None),
                    () = tokio::time::sleep(*delay) => {}
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// Why a program couldn't be started, and what to do about it.
fn not_started_message(program: &Path, e: &std::io::Error) -> String {
    let shown = program.display();
    let name = program
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| shown.to_string());
    let variable = if name.contains("ffprobe") {
        "FFPROBE_PATH"
    } else {
        "FFMPEG_PATH"
    };
    match e.kind() {
        std::io::ErrorKind::NotFound => format!(
            "{name} wasn't found at \"{shown}\". Install ffmpeg, or set {variable} to where it is."
        ),
        std::io::ErrorKind::PermissionDenied => format!(
            "{name} at \"{shown}\" couldn't be started because it isn't allowed to run. Check \
             its permissions, or set {variable} to another copy."
        ),
        _ => format!(
            "{name} at \"{shown}\" couldn't be started because {}.",
            chrysopoeia_core::plain::io_reason(e)
        ),
    }
}

/// `text` with its first letter in lower case, unless it starts with a name
/// or acronym ("NVIDIA", "Intel/AMD").
pub(crate) fn lower_first(text: &str) -> String {
    let first = text.split_whitespace().next().unwrap_or("");
    let keep = (first.len() > 1 && first.chars().skip(1).any(char::is_uppercase))
        || ["Intel", "Intel/AMD", "AMD", "NVIDIA"].contains(&first);
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if !keep => c.to_lowercase().chain(chars).collect(),
        _ => text.to_string(),
    }
}

/// Remove a leading `[context @ 0x…] ` and `[level] ` from an ffmpeg log line.
pub fn strip_log_prefix(line: &str) -> &str {
    split_log_line(line).2
}

/// Split an ffmpeg log line (as printed with `-loglevel level+…`) into its
/// context (e.g. `h264`), level (e.g. `error`) and message.
pub fn split_log_line(line: &str) -> (Option<&str>, Option<&str>, &str) {
    let mut rest = line.trim_start();
    let mut context = None;
    let mut level = None;
    for _ in 0..2 {
        let Some(inner) = rest.strip_prefix('[') else {
            break;
        };
        let Some(close) = inner.find(']') else {
            break;
        };
        let tag = &inner[..close];
        let after = inner[close + 1..].trim_start();
        if let Some((name, _addr)) = tag.split_once(" @ ") {
            if context.is_some() {
                break;
            }
            context = Some(name.trim());
        } else if is_log_level(tag) {
            level = Some(tag);
            rest = after;
            break;
        } else {
            break;
        }
        rest = after;
    }
    (context, level, rest)
}

fn is_log_level(tag: &str) -> bool {
    matches!(
        tag,
        "quiet" | "panic" | "fatal" | "error" | "warning" | "info" | "verbose" | "debug" | "trace"
    )
}

/// Split raw output bytes into lines. ffmpeg uses `\r` for in-place status
/// lines, so both `\r` and `\n` end a line. Invalid UTF-8 (odd file names) is
/// replaced rather than rejected.
pub fn split_output_lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .split(['\r', '\n'])
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// How an ffmpeg run ended.
#[derive(Debug, Clone, PartialEq)]
pub enum FfmpegExit {
    /// Exit status 0.
    Success {
        /// Last stderr lines.
        tail: String,
    },
    /// Non-zero exit status.
    Failed {
        /// Exit code; `None` when killed by a signal.
        code: Option<i32>,
        /// Last stderr lines.
        tail: String,
    },
    /// Stopped because it printed nothing for the stall timeout.
    Stalled {
        /// The stall timeout that was exceeded.
        after: Duration,
        /// Last stderr lines.
        tail: String,
    },
    /// Stopped because the cancellation token fired.
    Cancelled,
    /// The process could not be started at all.
    NotStarted {
        /// Why and what to do, in plain language, e.g. "ffmpeg wasn't found
        /// at "/usr/bin/ffmpeg". Install ffmpeg, or set FFMPEG_PATH to where
        /// it is."
        error: String,
    },
}

impl FfmpegExit {
    /// Plain-language description of a failed run, for error messages:
    /// "ffmpeg stopped with an error: "Invalid data found when processing
    /// input"", or for a well-known cause its explanation ("ffmpeg stopped
    /// with an error: the disk ran out of space while writing the new file").
    pub fn describe_failure(&self, program: &str) -> Option<String> {
        match self {
            Self::Success { .. } | Self::Cancelled => None,
            Self::Failed { code, tail } => {
                let reason = failure_reason(tail.lines())
                    .map(|r| chrysopoeia_core::plain::strip_os_error(&r))
                    .filter(|r| !r.is_empty())
                    .map(|r| match explain_failure(&r) {
                        Some(hint) => lower_first(hint),
                        None => format!("\"{}\"", r.trim_end_matches('.')),
                    });
                let status = match code {
                    Some(_) => "stopped with an error",
                    None => "was stopped by the system",
                };
                Some(match reason {
                    Some(r) => format!("{program} {status}: {r}"),
                    None => format!("{program} {status}"),
                })
            }
            Self::Stalled { after, .. } => Some(format!(
                "{program} stopped responding for {} minutes and was stopped",
                after.as_secs().div_ceil(60)
            )),
            Self::NotStarted { error } => Some(error.clone()),
        }
    }

    /// The captured stderr tail, if any.
    pub fn tail(&self) -> Option<&str> {
        match self {
            Self::Success { tail } | Self::Failed { tail, .. } | Self::Stalled { tail, .. } => {
                Some(tail.as_str())
            }
            Self::Cancelled | Self::NotStarted { .. } => None,
        }
    }
}

/// One process to run with [`run_ffmpeg`].
#[derive(Debug, Clone, Copy)]
pub struct FfmpegCommand<'a> {
    /// The ffmpeg (or ffprobe) binary.
    pub program: &'a Path,
    /// Arguments after the binary.
    pub args: &'a [String],
    /// Run under `nice -n 10` when a `nice` binary exists.
    pub low_priority: bool,
    /// Stop the process when it prints nothing for this long.
    pub stall_timeout: Duration,
}

impl<'a> FfmpegCommand<'a> {
    /// A normal-priority command with the default stall timeout.
    pub fn new(program: &'a Path, args: &'a [String]) -> Self {
        Self {
            program,
            args,
            low_priority: false,
            stall_timeout: DEFAULT_STALL_TIMEOUT,
        }
    }
}

/// Run ffmpeg to completion.
///
/// `on_progress` receives every `-progress` block printed on stdout and
/// `on_stderr` every stderr line (already split and decoded). Never panics;
/// every problem is reported through [`FfmpegExit`].
pub async fn run_ffmpeg(
    cmd: &FfmpegCommand<'_>,
    cancel: &CancellationToken,
    on_progress: &mut (dyn FnMut(&ProgressBlock) + Send),
    on_stderr: &mut (dyn FnMut(&str) + Send),
) -> FfmpegExit {
    if cancel.is_cancelled() {
        return FfmpegExit::Cancelled;
    }
    let nice = if cmd.low_priority {
        nice_binary().await
    } else {
        None
    };
    // `nice` starts even when the program it should run can't, and then
    // only its own exit code and message would tell. Check first, so a
    // missing ffmpeg is reported the same way with or without low priority.
    if nice.is_some()
        && let Err(e) = check_runnable(cmd.program).await
    {
        return FfmpegExit::NotStarted {
            error: not_started_message(cmd.program, &e),
        };
    }
    let mut command = match nice {
        Some(nice) => {
            let mut c = Command::new(nice);
            c.arg("-n").arg(NICE_LEVEL).arg(cmd.program);
            c
        }
        None => Command::new(cmd.program),
    };
    command
        .args(cmd.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    chrysopoeia_core::process::end_with_parent(command.as_std_mut());

    let spawned = spawn_patiently(
        || command.spawn(),
        &chrysopoeia_core::process::SPAWN_RETRY_DELAYS,
        cancel,
    )
    .await;
    let mut child = match spawned {
        Ok(Some(child)) => child,
        Ok(None) => return FfmpegExit::Cancelled,
        Err(e) => {
            let error = not_started_message(cmd.program, &e);
            return FfmpegExit::NotStarted { error };
        }
    };

    let mut stdout = child.stdout.take().map(|s| BufReader::new(s).split(b'\n'));
    let mut stderr = child.stderr.take().map(|s| BufReader::new(s).split(b'\n'));
    let mut parser = ProgressParser::new();
    let mut tail = StderrTail::default();
    let mut liveness = Liveness::default();
    let stall = tokio::time::sleep(cmd.stall_timeout);
    tokio::pin!(stall);

    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                drop((stdout.take(), stderr.take()));
                kill_child(&mut child).await;
                return FfmpegExit::Cancelled;
            }
            () = &mut stall => {
                drop((stdout.take(), stderr.take()));
                tracing::warn!(program = %cmd.program.display(), "ffmpeg made no progress for {:?}; stopping it", cmd.stall_timeout);
                kill_child(&mut child).await;
                return FfmpegExit::Stalled { after: cmd.stall_timeout, tail: tail.joined() };
            }
            segment = next_segment(&mut stdout), if stdout.is_some() => match segment {
                Some(bytes) => {
                    let mut alive = false;
                    for line in split_output_lines(&bytes) {
                        if let Some(block) = parser.push_line(&line) {
                            alive |= liveness.block(&block);
                            on_progress(&block);
                        } else if !line.contains('=') {
                            // Not part of a progress block: output of its own.
                            alive = true;
                        }
                    }
                    if alive {
                        stall.as_mut().reset(Instant::now() + cmd.stall_timeout);
                    }
                }
                None => stdout = None,
            },
            segment = next_segment(&mut stderr), if stderr.is_some() => match segment {
                Some(bytes) => {
                    let mut alive = false;
                    for line in split_output_lines(&bytes) {
                        alive |= liveness.stderr(&line);
                        on_stderr(&line);
                        tail.push(&line);
                    }
                    if alive {
                        stall.as_mut().reset(Instant::now() + cmd.stall_timeout);
                    }
                }
                None => stderr = None,
            },
            status = child.wait(), if stdout.is_none() && stderr.is_none() => {
                return match status {
                    Ok(s) if s.success() => FfmpegExit::Success { tail: tail.joined() },
                    Ok(s) => match nice.and_then(|n| nice_could_not_run(n, s.code(), &tail)) {
                        Some(e) => FfmpegExit::NotStarted { error: not_started_message(cmd.program, &e) },
                        None => FfmpegExit::Failed { code: s.code(), tail: tail.joined() },
                    },
                    Err(e) => FfmpegExit::Failed { code: None, tail: format!("{}\n{e}", tail.joined()) },
                };
            }
        }
    }
}

/// Next `\n`-terminated chunk from a reader; `None` at EOF or on a read
/// error. Cancel-safe (`Split::next_segment` keeps partial data).
async fn next_segment<R: AsyncBufRead + Unpin>(reader: &mut Option<Split<R>>) -> Option<Vec<u8>> {
    match reader {
        Some(r) => r.next_segment().await.ok().flatten(),
        None => std::future::pending().await,
    }
}

/// Kill the child at once and reap it. Its output is discarded, so a
/// graceful stop would only keep the caller (and a user pressing Cancel)
/// waiting while the encoder flushes frames nobody will use.
///
/// A process stuck in a read from a share that stopped answering may not
/// die until the share answers (the kernel holds it), so the wait for it is
/// bounded: past [`REAP_WAIT`] it is left to be reaped in the background.
async fn kill_child(child: &mut Child) {
    if let Err(e) = child.start_kill() {
        tracing::debug!("could not kill ffmpeg: {e}");
    }
    match tokio::time::timeout(REAP_WAIT, child.wait()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => tracing::debug!("could not reap ffmpeg: {e}"),
        Err(_) => tracing::warn!(
            "ffmpeg didn't end within {} seconds of being stopped (its files may be on a share \
             that isn't responding); it is left to end by itself",
            REAP_WAIT.as_secs()
        ),
    }
}

/// How long a killed ffmpeg may take to end before it is left to end by
/// itself. One that has its files on a share that stopped answering can't
/// end until the share answers (closing a file there waits for it); Cancel
/// and Stop don't wait for that.
const REAP_WAIT: Duration = Duration::from_secs(2);

static NICE_BINARY: OnceLock<Option<PathBuf>> = OnceLock::new();

/// The `nice` binary, looked up on `PATH` once and cached.
pub async fn nice_binary() -> Option<&'static Path> {
    if let Some(found) = NICE_BINARY.get() {
        return found.as_deref();
    }
    let found = tokio::task::spawn_blocking(|| find_in_path("nice"))
        .await
        .ok()
        .flatten();
    NICE_BINARY.get_or_init(|| found).as_deref()
}

/// Whether `program` (a path, or a bare name looked up on `PATH`) is a file
/// this process may run. The error is what starting it directly would give.
async fn check_runnable(program: &Path) -> std::io::Result<()> {
    let program = program.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let bare = program
            .parent()
            .is_none_or(|parent| parent.as_os_str().is_empty());
        let path = if bare {
            let name = program.to_string_lossy();
            find_in_path(&name).ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?
        } else {
            program
        };
        if std::fs::metadata(&path)?.is_dir() {
            return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        }
        may_execute(&path)
    })
    .await
    .unwrap_or(Ok(()))
}

#[cfg(unix)]
fn may_execute(path: &Path) -> std::io::Result<()> {
    rustix::fs::access(path, rustix::fs::Access::EXEC_OK)
        .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))
}

#[cfg(not(unix))]
fn may_execute(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// When `nice` itself says it couldn't run the program (exit code 126 when
/// it isn't allowed to run, 127 when it isn't there), the matching error.
fn nice_could_not_run(nice: &Path, code: Option<i32>, tail: &StderrTail) -> Option<std::io::Error> {
    let kind = match code? {
        126 => std::io::ErrorKind::PermissionDenied,
        127 => std::io::ErrorKind::NotFound,
        _ => return None,
    };
    let first = tail.lines().next()?;
    let from_nice =
        first.starts_with(&format!("{}:", nice.display())) || first.starts_with("nice:");
    from_nice.then(|| std::io::Error::from(kind))
}

/// Look for an executable file named `name` in the `PATH` directories.
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// The command line for display: program and arguments, shell-quoted where
/// needed so it can be pasted into a terminal.
pub fn display_command(program: &Path, args: &[String]) -> String {
    let program = program.to_string_lossy();
    std::iter::once(shell_quote(&program))
        .chain(args.iter().map(|a| shell_quote(a)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Quote one argument for a POSIX shell.
pub fn shell_quote(arg: &str) -> Cow<'_, str> {
    let safe = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=+,@%^".contains(c));
    if safe {
        Cow::Borrowed(arg)
    } else {
        Cow::Owned(format!("'{}'", arg.replace('\'', r"'\''")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// libx265 under Docker's seccomp profile: libnuma's complaints never
    /// reach the log tail or the error message.
    #[test]
    fn numa_noise_is_ignored() {
        let mut tail = StderrTail::new(5);
        for _ in 0..20 {
            tail.push("set_mempolicy: Operation not permitted");
        }
        tail.push("[libx265 @ 0x1] [error] Cannot open libx265 encoder.");
        tail.push("mbind: Operation not permitted");
        assert_eq!(
            tail.joined(),
            "[libx265 @ 0x1] [error] Cannot open libx265 encoder."
        );
        let untagged = ["set_mempolicy: Operation not permitted"; 3];
        assert_eq!(failure_reason(untagged.into_iter()), None);
    }

    #[test]
    fn input_damage_lines() {
        assert!(is_input_damage(
            "[matroska,webm @ 0x55fb] [error] File ended prematurely"
        ));
        assert!(is_input_damage(
            "[h264 @ 0x1] [error] error while decoding MB 3 4, bytestream -5"
        ));
        assert!(is_input_damage(
            "[mov @ 0x1] [warning] Packet corrupt (stream = 0, dts = 9)"
        ));
        assert!(!is_input_damage(
            "[libx265 @ 0x1] [warning] some unrelated tuning note"
        ));
        assert!(!is_input_damage(
            "[h264 @ 0x1] [info] concealing 3 DC, 3 AC, 3 MV errors"
        ));
        assert!(!is_input_damage("set_mempolicy: Operation not permitted"));
    }

    const FIXTURE: &str = "frame=0\nfps=0.00\nstream_0_0_q=0.0\nbitrate=  82.9kbits/s\n\
total_size=1203\nout_time_us=116100\nout_time_ms=116100\nout_time=00:00:00.116100\n\
dup_frames=0\ndrop_frames=0\nspeed=1.52x\nprogress=continue\n\
frame=131\nfps=121.17\nstream_0_0_q=27.0\nbitrate=3225.6kbits/s\ntotal_size=2359296\n\
out_time_us=5851429\nout_time_ms=5851429\nout_time=00:00:05.851429\ndup_frames=0\n\
drop_frames=0\nspeed=5.41x\nprogress=continue\n\
frame=144\nfps=129.28\nbitrate=3974.0kbits/s\ntotal_size=2975915\nout_time_us=6000000\n\
out_time_ms=6000000\nout_time=00:00:06.000000\nspeed=5.39x\nprogress=end\n";

    fn parse_all(text: &str) -> Vec<ProgressBlock> {
        let mut parser = ProgressParser::new();
        text.lines().filter_map(|l| parser.push_line(l)).collect()
    }

    #[test]
    fn parses_progress_blocks() {
        let blocks = parse_all(FIXTURE);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].frame, Some(0));
        assert_eq!(blocks[0].out_time_secs, Some(0.1161));
        assert_eq!(blocks[0].speed, Some(1.52));
        assert_eq!(blocks[0].total_size, Some(1203));
        assert!(!blocks[0].end);
        assert_eq!(blocks[1].fps, Some(121.17));
        assert!((blocks[1].out_time_secs.unwrap() - 5.851429).abs() < 1e-9);
        assert!(blocks[2].end);
        assert_eq!(blocks[2].out_time_secs, Some(6.0));
    }

    #[test]
    fn out_time_ms_is_microseconds() {
        let blocks = parse_all("out_time_ms=2500000\nprogress=continue\n");
        assert_eq!(blocks[0].out_time_secs, Some(2.5));
    }

    #[test]
    fn handles_not_available_values() {
        let text = "frame=0\nfps=N/A\nout_time_us=N/A\nout_time_ms=N/A\n\
out_time=N/A\ntotal_size=N/A\nspeed=N/A\nprogress=continue\n";
        let blocks = parse_all(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].out_time_secs, None);
        assert_eq!(blocks[0].fps, None);
        assert_eq!(blocks[0].speed, None);
        assert_eq!(blocks[0].total_size, None);
    }

    #[test]
    fn negative_out_time_is_zero() {
        let blocks = parse_all("out_time_us=-23000\nprogress=continue\n");
        assert_eq!(blocks[0].out_time_secs, Some(0.0));
    }

    #[test]
    fn falls_back_to_clock_out_time() {
        let blocks = parse_all("out_time=01:02:03.500000\nprogress=continue\n");
        assert_eq!(blocks[0].out_time_secs, Some(3723.5));
    }

    #[test]
    fn speed_values() {
        assert_eq!(parse_speed("2.31x"), Some(2.31));
        assert_eq!(parse_speed(" 0.5x"), Some(0.5));
        assert_eq!(parse_speed("N/A"), None);
        assert_eq!(parse_speed("0x"), None);
        assert_eq!(parse_speed(""), None);
    }

    #[test]
    fn progress_percent_and_eta() {
        let block = ProgressBlock {
            out_time_secs: Some(30.0),
            speed: Some(2.0),
            fps: Some(48.0),
            ..Default::default()
        };
        let p = compute_progress(&block, Some(120.0), 15.0);
        assert!((p.percent - 25.0).abs() < 1e-4);
        assert_eq!(p.eta_secs, Some(45));
        assert_eq!(p.fps, Some(48.0));

        // No speed: estimate from wall-clock rate (30 s of media in 60 s).
        let slow = ProgressBlock {
            out_time_secs: Some(30.0),
            ..Default::default()
        };
        assert_eq!(
            compute_progress(&slow, Some(120.0), 60.0).eta_secs,
            Some(180)
        );

        // Unknown duration: no percent, no ETA, and it says so.
        let p = compute_progress(&block, None, 15.0);
        assert_eq!(p.percent, 0.0);
        assert_eq!(p.eta_secs, None);
        assert_eq!(p.basis, ProgressBasis::Unknown);

        // Overshoot is clamped; the end block is always 100 %.
        let over = ProgressBlock {
            out_time_secs: Some(130.0),
            ..Default::default()
        };
        assert_eq!(compute_progress(&over, Some(120.0), 1.0).percent, 100.0);
        let end = ProgressBlock {
            end: true,
            ..Default::default()
        };
        let p = compute_progress(&end, Some(120.0), 1.0);
        assert_eq!(p.percent, 100.0);
        assert_eq!(p.eta_secs, Some(0));
        assert_eq!(p.basis, ProgressBasis::Time);
    }

    /// With the output time reported, progress is the time over the length,
    /// as before, and says so.
    #[test]
    fn progress_from_output_time_is_measured() {
        let block = ProgressBlock {
            frame: Some(720),
            out_time_secs: Some(30.0),
            speed: Some(2.0),
            fps: Some(48.0),
            ..Default::default()
        };
        let timeline = Timeline {
            duration_secs: Some(120.0),
            expected_frames: Some(2880.0),
        };
        let p = estimate_progress(&block, timeline, 15.0);
        assert_eq!(p.basis, ProgressBasis::Time);
        assert!((p.percent - 25.0).abs() < 1e-4);
        assert_eq!(p.eta_secs, Some(45));
        assert_eq!(p.frames, Some(720));
    }

    /// The Atlas CPU fallback: ffmpeg 7 printed frames and fps for minutes
    /// but no output time (`out_time=N/A`), and progress sat at 0 % with no
    /// time left. Frames over the frames the source should have give an
    /// estimate instead, labelled as one, with the time left at that pace.
    #[test]
    fn progress_without_output_time_is_estimated_from_frames() {
        let text = "frame=366\nfps=3.66\nstream_0_0_q=28.0\nbitrate=N/A\ntotal_size=4096\n\
out_time_us=N/A\nout_time_ms=N/A\nout_time=N/A\ndup_frames=0\ndrop_frames=0\nspeed=N/A\n\
progress=continue\n";
        let block = parse_all(text).remove(0);
        assert_eq!(block.out_time_secs, None);
        // 61.1 s at 23.976 fps: 1465 frames.
        let timeline = Timeline {
            duration_secs: Some(61.1),
            expected_frames: Some(61.1 * 24000.0 / 1001.0),
        };
        let p = estimate_progress(&block, timeline, 100.0);
        assert_eq!(p.basis, ProgressBasis::Frames);
        assert!((p.percent - 24.98).abs() < 0.05, "{}", p.percent);
        assert_eq!(p.frames, Some(366));
        assert_eq!(p.fps, Some(3.66));
        // 1099 frames left at 3.66 fps.
        assert_eq!(p.eta_secs, Some(300));

        // Without a usable frame count to compare with, it is unknown: no
        // percentage that only looks like one, and no time left.
        for expected in [None, Some(0.0), Some(f64::NAN)] {
            let timeline = Timeline {
                expected_frames: expected,
                ..timeline
            };
            let p = estimate_progress(&block, timeline, 100.0);
            assert_eq!(p.basis, ProgressBasis::Unknown, "{expected:?}");
            assert_eq!((p.percent, p.eta_secs), (0.0, None));
            assert_eq!(p.frames, Some(366), "the frames are still reported");
        }
        // The old function has no frame count to go by.
        let p = compute_progress(&block, Some(61.1), 100.0);
        assert_eq!(
            (p.basis, p.percent, p.eta_secs),
            (ProgressBasis::Unknown, 0.0, None)
        );
        assert_eq!(p.frames, Some(366));
    }

    /// Frames can't tell that the end is reached: an estimate stays below
    /// 100 % until ffmpeg's final block, and more frames than the source
    /// should have mean the expected count was wrong, so it is dropped.
    #[test]
    fn frame_estimate_is_capped_and_dropped_when_wrong() {
        let timeline = Timeline {
            duration_secs: Some(10.0),
            expected_frames: Some(240.0),
        };
        let at = |frame: u64| {
            let block = ProgressBlock {
                frame: Some(frame),
                fps: Some(24.0),
                ..Default::default()
            };
            estimate_progress(&block, timeline, 10.0)
        };
        assert_eq!(at(240).basis, ProgressBasis::Frames);
        assert_eq!(at(240).percent, 99.0);
        assert_eq!(at(250).percent, 99.0, "a few frames over is rounding");
        assert_eq!(at(300).basis, ProgressBasis::Unknown);
        assert_eq!((at(300).percent, at(300).eta_secs), (0.0, None));
        // No frames yet: 0 %, honestly, with no time left to guess.
        let start = at(0);
        assert_eq!((start.basis, start.percent), (ProgressBasis::Frames, 0.0));
        assert_eq!(start.eta_secs, None);
    }

    /// ffmpeg 7 reports the output time of the track furthest behind: a
    /// subtitle track whose last line was at 0:05 holds it there while the
    /// video is at 0:44. The frames, well ahead, are the better measure.
    #[test]
    fn output_time_held_back_by_a_sparse_track_gives_way_to_frames() {
        let timeline = Timeline {
            duration_secs: Some(100.0),
            expected_frames: Some(2400.0),
        };
        let block = ProgressBlock {
            frame: Some(1056),
            out_time_secs: Some(5.0),
            fps: Some(48.0),
            speed: Some(0.2),
            ..Default::default()
        };
        let p = estimate_progress(&block, timeline, 22.0);
        assert_eq!(p.basis, ProgressBasis::Frames);
        assert!((p.percent - 44.0).abs() < 1e-3, "{}", p.percent);
        assert_eq!(p.eta_secs, Some(28));
        // Close to each other, the output time stays the measure.
        let block = ProgressBlock {
            out_time_secs: Some(43.0),
            ..block
        };
        assert_eq!(
            estimate_progress(&block, timeline, 22.0).basis,
            ProgressBasis::Time
        );
    }

    /// ffmpeg before 6 printed "no time yet" as its lowest number; that is
    /// no output time, not the start of the file.
    #[test]
    fn old_ffmpeg_no_time_yet_is_no_output_time() {
        let text = "frame=12\nout_time_us=-9223372036854775807\n\
out_time_ms=-9223372036854775807\nout_time=-2562047:47:16.854775\nprogress=continue\n";
        assert_eq!(parse_all(text)[0].out_time_secs, None);
        let blocks = parse_all("out_time=-00:00:00.023000\nprogress=continue\n");
        assert_eq!(blocks[0].out_time_secs, Some(0.0));
    }

    #[test]
    fn stderr_tail_keeps_last_lines() {
        let mut tail = StderrTail::new(3);
        for i in 0..5 {
            tail.push(&format!("line {i}"));
        }
        tail.push("   ");
        assert_eq!(
            tail.lines().collect::<Vec<_>>(),
            ["line 2", "line 3", "line 4"]
        );
        assert_eq!(tail.joined(), "line 2\nline 3\nline 4");
    }

    #[test]
    fn meaningful_line_skips_noise_and_prefixes() {
        let mut tail = StderrTail::default();
        tail.push("[hevc_nvenc @ 0x55d1c2a4b940] OpenEncodeSessionEx failed: unsupported device (2): (no details)");
        tail.push("[vost#0:0/hevc_nvenc @ 0x55d1c2a4a1c0] Error initializing output stream: Error while opening encoder");
        tail.push("Conversion failed!");
        assert_eq!(
            tail.last_meaningful().as_deref(),
            Some("OpenEncodeSessionEx failed: unsupported device (2): (no details)")
        );
        assert_eq!(StderrTail::default().last_meaningful(), None);
    }

    /// Real tail of an NVENC encode in a container without a GPU, as printed
    /// at the default log level (the AAC encoder's statistics come last).
    const NVENC_TAIL_INFO: &str = "\
[h264_nvenc @ 0x564f63ea1780] Cannot load libcuda.so.1
[vost#0:0/h264_nvenc @ 0x564f63ea14c0] Error while opening encoder - maybe incorrect parameters such as bit_rate, rate, width or height.
Error while filtering: Operation not permitted
[out#0/matroska @ 0x564f63e9f6c0] Nothing was written into output file, because at least one of its streams received no packets.
frame=    0 fps=0.0 q=0.0 Lsize=       0kB time=00:00:00.16 bitrate=   0.0kbits/s speed= 1.8x
[aac @ 0x564f63e97a40] Qavg: 326.719
[aac @ 0x564f63f14d40] Qavg: 326.008
Conversion failed!";

    /// The same failure with `-loglevel level+warning`.
    const NVENC_TAIL_TAGGED: &str = "\
[h264_nvenc @ 0x559164be8e40] [error] Cannot load libcuda.so.1
[vost#0:0/h264_nvenc @ 0x559164cda600] [error] Error while opening encoder - maybe incorrect parameters such as bit_rate, rate, width or height.
[error] Error while filtering: Operation not permitted
[out#0/matroska @ 0x559164bd0ec0] [error] Nothing was written into output file, because at least one of its streams received no packets.";

    /// Real tail of an encode whose temp disk filled up (default log level).
    const DISK_FULL_TAIL_INFO: &str = "\
[out#0/matroska @ 0x561cc0924300] Error writing trailer: No space left on device
[out#0/matroska @ 0x561cc0924300] Error closing file: No space left on device
[out#0/matroska @ 0x561cc0924300] video:5232kB audio:105kB subtitle:0kB other streams:0kB global headers:0kB muxing overhead: unknown
frame=   43 fps=0.0 q=-1.0 Lsize=    3072kB time=00:00:02.34 bitrate=10729.9kbits/s speed=2.88x
[libx264 @ 0x561cc09605c0] frame I:1     Avg QP: 7.00  size:156097
[libx264 @ 0x561cc09605c0] frame P:52    Avg QP: 6.46  size:124802
[libx264 @ 0x561cc09605c0] mb I  I16..4: 100.0%  0.0%  0.0%
[libx264 @ 0x561cc09605c0] mb P  I16..4:  4.2%  0.0%  0.0%  P16..4:  9.7%  0.0%  0.0%  0.0%  0.0%    skip:86.1%
[libx264 @ 0x561cc09605c0] coded y,uvDC,uvAC intra: 11.8% 18.0% 17.4% inter: 8.1% 9.3% 9.2%
[libx264 @ 0x561cc09605c0] i16 v,h,dc,p: 91%  5%  3%  0%
[libx264 @ 0x561cc09605c0] i8c dc,h,v,p: 79%  8% 13%  0%
[libx264 @ 0x561cc09605c0] kb/s:24075.42
[aac @ 0x561cc090bf40] Qavg: 156.151
Conversion failed!";

    /// The same failure with `-loglevel level+warning`.
    const DISK_FULL_TAIL_TAGGED: &str = "\
[vost#0:0/libx264 @ 0x55a55acc1180] [error] Error submitting a packet to the muxer: No space left on device
[out#0/matroska @ 0x55a55ac84f00] [error] Error muxing a packet
[out#0/matroska @ 0x55a55ac84f00] [error] Error writing trailer: No space left on device
[out#0/matroska @ 0x55a55ac84f00] [error] Error closing file: No space left on device";

    #[test]
    fn failure_reasons_from_real_tails() {
        for tail in [NVENC_TAIL_INFO, NVENC_TAIL_TAGGED] {
            assert_eq!(
                failure_reason(tail.lines()).as_deref(),
                Some("Cannot load libcuda.so.1")
            );
            let exit = FfmpegExit::Failed {
                code: Some(255),
                tail: tail.into(),
            };
            assert_eq!(
                exit.describe_failure("ffmpeg").as_deref(),
                Some(
                    "ffmpeg stopped with an error: the NVIDIA driver could not be loaded. \
                     Check that the GPU is passed through to the container"
                )
            );
        }
        assert_eq!(
            failure_reason(DISK_FULL_TAIL_INFO.lines()).as_deref(),
            Some("Error writing trailer: No space left on device")
        );
        assert_eq!(
            failure_reason(DISK_FULL_TAIL_TAGGED.lines()).as_deref(),
            Some("Error submitting a packet to the muxer: No space left on device")
        );
        let exit = FfmpegExit::Failed {
            code: Some(228),
            tail: DISK_FULL_TAIL_TAGGED.into(),
        };
        let message = exit.describe_failure("ffmpeg").unwrap();
        assert_eq!(
            message,
            "ffmpeg stopped with an error: the disk ran out of space while writing the new file"
        );
    }

    #[test]
    fn decoder_damage_ranks_below_the_real_cause() {
        let tail = "[h264 @ 0x1] [error] error while decoding MB 76 4\n\
                    [h264_vaapi @ 0x2] [error] Failed to upload frame: Input/output error\n\
                    [error] Conversion failed!";
        assert_eq!(
            failure_reason(tail.lines()).as_deref(),
            Some("Failed to upload frame: Input/output error")
        );
        // Only damage and consequences: damage it is.
        let tail = "[h264 @ 0x1] [error] error while decoding MB 76 4\nConversion failed!";
        assert_eq!(
            failure_reason(tail.lines()).as_deref(),
            Some("error while decoding MB 76 4")
        );
        // Untagged notices are not errors; the last non-statistics line is used.
        assert_eq!(
            failure_reason("Some notice\n[aac @ 0x1] Qavg: 1.0".lines()).as_deref(),
            Some("Some notice")
        );
    }

    #[test]
    fn splits_log_lines() {
        assert_eq!(
            split_log_line("[h264 @ 0x5600295240c0] [error] cbp too large (353) at 76 4"),
            (Some("h264"), Some("error"), "cbp too large (353) at 76 4")
        );
        assert_eq!(
            split_log_line("[info] Input #0, matroska,webm, from 'x.mkv':"),
            (None, Some("info"), "Input #0, matroska,webm, from 'x.mkv':")
        );
        assert_eq!(
            split_log_line("[h264 @ 0x1] error while decoding MB 76 4"),
            (Some("h264"), None, "error while decoding MB 76 4")
        );
        assert_eq!(split_log_line("plain text"), (None, None, "plain text"));
        assert_eq!(
            split_log_line("[Parsed_ssim_0 @ 0x1] [info] SSIM Y:0.99"),
            (Some("Parsed_ssim_0"), Some("info"), "SSIM Y:0.99")
        );
    }

    #[test]
    fn splits_carriage_returns_and_bad_utf8() {
        let lines = split_output_lines(b"frame=1\rframe=2\r\nname=caf\xe9\n\n");
        assert_eq!(lines, ["frame=1", "frame=2", "name=caf\u{fffd}"]);
    }

    #[test]
    fn quotes_commands() {
        let args = vec![
            "-i".to_string(),
            "/media/Movie (2020)/It's here.mkv".to_string(),
            "-c:v".to_string(),
            "libx264".to_string(),
            String::new(),
        ];
        assert_eq!(
            display_command(Path::new("/usr/bin/ffmpeg"), &args),
            r"/usr/bin/ffmpeg -i '/media/Movie (2020)/It'\''s here.mkv' -c:v libx264 ''"
        );
    }

    #[test]
    fn describes_failures() {
        let failed = FfmpegExit::Failed {
            code: Some(1),
            tail: "[libx264 @ 0x1] Unknown option\nConversion failed!".into(),
        };
        assert_eq!(
            failed.describe_failure("ffmpeg").as_deref(),
            Some("ffmpeg stopped with an error: \"Unknown option\"")
        );
        let killed = FfmpegExit::Failed {
            code: None,
            tail: "Error opening input: Permission denied (os error 13)".into(),
        };
        assert_eq!(
            killed.describe_failure("ffmpeg").as_deref(),
            Some("ffmpeg was stopped by the system: \"Error opening input: Permission denied\"")
        );
        let stalled = FfmpegExit::Stalled {
            after: Duration::from_secs(600),
            tail: String::new(),
        };
        assert_eq!(
            stalled.describe_failure("ffmpeg").as_deref(),
            Some("ffmpeg stopped responding for 10 minutes and was stopped")
        );
        assert_eq!(FfmpegExit::Cancelled.describe_failure("ffmpeg"), None);
    }

    #[tokio::test]
    async fn missing_binary_is_not_started() {
        let args = vec!["-version".to_string()];
        let program = Path::new("/nonexistent/ffmpeg-for-tests");
        let exit = run_ffmpeg(
            &FfmpegCommand::new(program, &args),
            &CancellationToken::new(),
            &mut |_| {},
            &mut |_| {},
        )
        .await;
        assert!(matches!(exit, FfmpegExit::NotStarted { .. }), "{exit:?}");
    }

    /// With low priority (the default) ffmpeg runs under `nice`, which
    /// starts fine even when ffmpeg can't; the result must still say that
    /// ffmpeg wasn't found or isn't allowed to run.
    #[cfg(unix)]
    #[tokio::test]
    async fn missing_binary_is_not_started_under_nice() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let not_runnable = dir.path().join("ffmpeg");
        std::fs::write(&not_runnable, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&not_runnable, std::fs::Permissions::from_mode(0o644)).unwrap();
        let args = vec!["-version".to_string()];
        let cases = [
            (
                Path::new("/nonexistent/ffmpeg-for-tests"),
                "wasn't found at",
            ),
            (Path::new("ffmpeg-not-on-path-for-tests"), "wasn't found at"),
            (not_runnable.as_path(), "isn't allowed to run"),
            (dir.path(), "isn't allowed to run"),
        ];
        for (program, expected) in cases {
            let mut cmd = FfmpegCommand::new(program, &args);
            cmd.low_priority = true;
            let exit = run_ffmpeg(&cmd, &CancellationToken::new(), &mut |_| {}, &mut |_| {}).await;
            match &exit {
                FfmpegExit::NotStarted { error } => {
                    assert!(error.contains(expected), "{program:?}: {error}");
                    assert!(!error.contains("nice"), "{error}");
                }
                other => panic!("{program:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn nice_reports_a_program_it_could_not_run() {
        let nice = Path::new("/usr/bin/nice");
        let tail = |line: &str| {
            let mut t = StderrTail::default();
            t.push(line);
            t
        };
        let missing = tail("/usr/bin/nice: '/x/ffmpeg': No such file or directory");
        assert_eq!(
            nice_could_not_run(nice, Some(127), &missing).map(|e| e.kind()),
            Some(std::io::ErrorKind::NotFound)
        );
        let busybox = tail("nice: can't execute '/x/ffmpeg': Permission denied");
        assert_eq!(
            nice_could_not_run(nice, Some(126), &busybox).map(|e| e.kind()),
            Some(std::io::ErrorKind::PermissionDenied)
        );
        // ffmpeg's own failures are not mistaken for nice's.
        let ffmpeg = tail("Error opening input file /x/in.mkv.");
        assert!(nice_could_not_run(nice, Some(127), &ffmpeg).is_none());
        assert!(nice_could_not_run(nice, Some(1), &missing).is_none());
        assert!(nice_could_not_run(nice, None, &missing).is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn runs_a_process_and_collects_output() {
        // `sh` stands in for ffmpeg: progress on stdout, messages on stderr.
        let script = "printf 'out_time_us=1000000\\nspeed=2.0x\\nprogress=continue\\n\
out_time_us=2000000\\nprogress=end\\n'; echo 'warning line' >&2; exit 3";
        let args = vec!["-c".to_string(), script.to_string()];
        let mut blocks = Vec::new();
        let mut errors = Vec::new();
        let exit = run_ffmpeg(
            &FfmpegCommand::new(Path::new("sh"), &args),
            &CancellationToken::new(),
            &mut |b| blocks.push(b.clone()),
            &mut |l| errors.push(l.to_string()),
        )
        .await;
        assert_eq!(
            exit,
            FfmpegExit::Failed {
                code: Some(3),
                tail: "warning line".into()
            }
        );
        assert_eq!(blocks.len(), 2);
        assert!(blocks[1].end);
        assert_eq!(errors, ["warning line"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_stops_the_process() {
        let args = vec!["-c".to_string(), "sleep 30".to_string()];
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            trigger.cancel();
        });
        let started = std::time::Instant::now();
        let mut cmd = FfmpegCommand::new(Path::new("sh"), &args);
        cmd.low_priority = true;
        let exit = run_ffmpeg(&cmd, &cancel, &mut |_| {}, &mut |_| {}).await;
        assert_eq!(exit, FfmpegExit::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// Encoders that ignore SIGTERM while they flush (x265, SVT-AV1) must
    /// not keep a cancelled job running: the process is killed outright,
    /// without asking it to stop first (a request it could ignore for as
    /// long as it likes). The script notes any SIGTERM it gets, so no
    /// timing is needed to tell; the time bound only shows the job doesn't
    /// wait for the minute the script would run.
    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_does_not_wait_for_a_process_ignoring_sigterm() {
        let dir = tempfile::tempdir().unwrap();
        let asked = dir.path().join("asked-to-stop");
        // `sleep & wait` so the shell runs its trap as soon as a TERM comes.
        let script = format!(
            "trap 'echo term > \"{}\"' TERM; echo ready >&2; sleep 30 & wait; sleep 30 & wait",
            asked.display()
        );
        let args = vec!["-c".to_string(), script];
        let cancel = CancellationToken::new();
        let cmd = FfmpegCommand::new(Path::new("sh"), &args);
        let trigger = cancel.clone();
        // Cancel once the script is running (its first line on stderr).
        let mut on_stderr = move |line: &str| {
            if line.contains("ready") {
                trigger.cancel();
            }
        };
        let started = std::time::Instant::now();
        let exit = run_ffmpeg(&cmd, &cancel, &mut |_| {}, &mut on_stderr).await;
        assert_eq!(exit, FfmpegExit::Cancelled);
        let took = started.elapsed();
        assert!(took < Duration::from_secs(20), "took {took:?}");
        // Give a trap that did fire time to write (it never should).
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !asked.exists(),
            "the process was asked to stop before it was killed"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn silent_process_counts_as_stalled() {
        let args = vec!["-c".to_string(), "sleep 30".to_string()];
        let mut cmd = FfmpegCommand::new(Path::new("sh"), &args);
        cmd.stall_timeout = Duration::from_millis(300);
        let exit = run_ffmpeg(&cmd, &CancellationToken::new(), &mut |_| {}, &mut |_| {}).await;
        assert!(matches!(exit, FfmpegExit::Stalled { .. }), "{exit:?}");
    }

    /// Running out of file handles is waited out; other errors are not.
    #[tokio::test]
    async fn starting_waits_while_too_many_files_are_open() {
        let delays = [Duration::from_millis(10); 3];
        let cancel = CancellationToken::new();
        let mut tries = 0;
        let started = spawn_patiently(
            || {
                tries += 1;
                if tries < 3 {
                    Err(std::io::Error::from_raw_os_error(24))
                } else {
                    Ok(tries)
                }
            },
            &delays,
            &cancel,
        )
        .await;
        assert_eq!(started.unwrap(), Some(3));

        let mut tries = 0;
        let never = spawn_patiently(
            || -> std::io::Result<()> {
                tries += 1;
                Err(std::io::Error::from_raw_os_error(24))
            },
            &delays,
            &cancel,
        )
        .await;
        assert_eq!(never.unwrap_err().raw_os_error(), Some(24));
        assert_eq!(tries, 4, "the first try and one per wait");

        let mut tries = 0;
        let missing = spawn_patiently(
            || -> std::io::Result<()> {
                tries += 1;
                Err(std::io::Error::from(std::io::ErrorKind::NotFound))
            },
            &delays,
            &cancel,
        )
        .await;
        assert!(missing.is_err());
        assert_eq!(tries, 1);

        cancel.cancel();
        let cancelled = spawn_patiently(
            || -> std::io::Result<()> { Err(std::io::Error::from_raw_os_error(23)) },
            &[Duration::from_secs(60)],
            &cancel,
        )
        .await;
        assert!(matches!(cancelled, Ok(None)));
    }

    #[test]
    fn only_blocks_that_move_count_as_progress() {
        let mut live = Liveness::default();
        let block = |frame: u64, secs: f64, size: u64| ProgressBlock {
            frame: Some(frame),
            out_time_secs: Some(secs),
            total_size: Some(size),
            fps: Some(24.0),
            speed: Some(1.0),
            end: false,
        };
        assert!(live.block(&block(0, 0.0, 0)), "the first block");
        assert!(live.block(&block(24, 1.0, 1000)));
        let mut stuck = block(24, 1.0, 1000);
        for speed in [0.9, 0.5, 0.1] {
            // ffmpeg 7 while stuck: the same position, a falling speed.
            stuck.speed = Some(speed);
            assert!(!live.block(&stuck));
        }
        assert!(live.block(&block(24, 1.0, 1200)), "bytes written");
        assert!(live.block(&block(25, 1.0, 1200)), "a frame");
        assert!(live.block(&ProgressBlock {
            end: true,
            ..block(25, 1.0, 1200)
        }));
        assert!(live.stderr("warning A"));
        assert!(!live.stderr("warning A"));
        assert!(live.stderr("warning B"));
    }

    /// The real thing: ffmpeg 7 (the Docker image ships jellyfin-ffmpeg 7)
    /// reading an input that stops delivering keeps printing the same
    /// progress block every half second; it still counts as stalled. Runs
    /// when `CHRYSOPOEIA_TEST_FFMPEG7` names an ffmpeg 7 binary with the
    /// libvpx encoder (Playwright's build has it); ffmpeg 6 makes the
    /// sample frames.
    #[cfg(unix)]
    #[tokio::test]
    async fn ffmpeg7_stuck_on_its_input_counts_as_stalled() {
        let Some(ffmpeg7) = std::env::var_os("CHRYSOPOEIA_TEST_FFMPEG7") else {
            eprintln!("CHRYSOPOEIA_TEST_FFMPEG7 not set; skipping");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let frames = dir.path().join("frames.mjpeg");
        let made = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=10",
            ])
            .args(["-frames:v", "60", "-c:v", "mjpeg", "-f", "image2pipe"])
            .arg(&frames)
            .status();
        if !made.is_ok_and(|s| s.success()) {
            eprintln!("ffmpeg can't make sample frames; skipping");
            return;
        }
        // A pipe that delivers the frames, then stays open and silent, like
        // a share that stopped answering mid-file.
        let fifo = dir.path().join("input");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        assert!(made.is_ok_and(|s| s.success()));
        let (fifo_w, frames_r) = (fifo.clone(), frames.clone());
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let writer = std::thread::spawn(move || {
            use std::io::Write as _;
            let Ok(mut pipe) = std::fs::OpenOptions::new().write(true).open(&fifo_w) else {
                return;
            };
            if pipe
                .write_all(&std::fs::read(&frames_r).unwrap_or_default())
                .is_ok()
            {
                let _ = done_rx.recv_timeout(Duration::from_secs(60));
            }
        });
        let out = dir.path().join("out.webm");
        let args: Vec<String> = [
            "-hide_banner",
            "-nostdin",
            "-y",
            "-nostats",
            "-progress",
            "pipe:1",
            "-readrate",
            "1",
            "-f",
            "image2pipe",
            "-c:v",
            "mjpeg",
            "-framerate",
            "10",
            "-i",
        ]
        .iter()
        .map(|a| (*a).to_string())
        .chain([fifo.to_string_lossy().into_owned()])
        .chain(["-c:v", "libvpx", "-f", "webm"].map(String::from))
        .chain([out.to_string_lossy().into_owned()])
        .collect();
        let program = PathBuf::from(ffmpeg7);
        let mut cmd = FfmpegCommand::new(&program, &args);
        cmd.stall_timeout = Duration::from_secs(4);
        let mut blocks = 0;
        let started = std::time::Instant::now();
        let exit = run_ffmpeg(
            &cmd,
            &CancellationToken::new(),
            &mut |_| blocks += 1,
            &mut |_| {},
        )
        .await;
        let _ = done_tx.send(());
        writer.join().unwrap();
        assert!(matches!(exit, FfmpegExit::Stalled { .. }), "{exit:?}");
        assert!(started.elapsed() < Duration::from_secs(40));
        // ffmpeg 7 kept printing while it was stuck (a block every half
        // second): about 12 while the frames came in, and more after.
        assert!(blocks >= 16, "{blocks} blocks");
    }

    /// ffmpeg 7 keeps printing progress blocks while it is stuck; a process
    /// that repeats the same block forever is hung all the same.
    ///
    /// The stall timeout is 80 times the pace of the output, so a machine
    /// many times slower than usual still delivers every block well within
    /// it: only the repeating counts against the process.
    #[cfg(unix)]
    #[tokio::test]
    async fn repeating_the_same_progress_block_counts_as_stalled() {
        const STALL: Duration = Duration::from_secs(4);
        let stuck = "while true; do printf 'frame=59\\nfps=0.0\\nout_time_us=5900000\\n\
total_size=48000\\nspeed=0.5x\\nprogress=continue\\n'; echo 'decoder error' >&2; \
sleep 0.05; done";
        let args = vec!["-c".to_string(), stuck.to_string()];
        let mut cmd = FfmpegCommand::new(Path::new("sh"), &args);
        cmd.stall_timeout = STALL;
        let mut blocks = 0;
        let started = std::time::Instant::now();
        let exit = run_ffmpeg(
            &cmd,
            &CancellationToken::new(),
            &mut |_| blocks += 1,
            &mut |_| {},
        )
        .await;
        assert!(matches!(exit, FfmpegExit::Stalled { .. }), "{exit:?}");
        // It kept printing (the same block) the whole time it was stuck.
        assert!(blocks > 3, "{blocks} blocks");
        // Stopped after the stall timeout, not left running (it never ends).
        assert!(started.elapsed() >= STALL, "{:?}", started.elapsed());
        assert!(started.elapsed() < STALL * 15, "{:?}", started.elapsed());

        // The same pace with the position moving is progress, however long
        // it takes in all (here more than the stall timeout).
        let working = "i=0; while [ $i -lt 100 ]; do i=$((i+1)); printf \"frame=$i\\n\
out_time_us=${i}00000\\nprogress=continue\\n\"; sleep 0.05; done";
        let args = vec!["-c".to_string(), working.to_string()];
        let mut cmd = FfmpegCommand::new(Path::new("sh"), &args);
        cmd.stall_timeout = STALL;
        let started = std::time::Instant::now();
        let exit = run_ffmpeg(&cmd, &CancellationToken::new(), &mut |_| {}, &mut |_| {}).await;
        assert!(matches!(exit, FfmpegExit::Success { .. }), "{exit:?}");
        assert!(started.elapsed() >= STALL, "{:?}", started.elapsed());
    }
}
