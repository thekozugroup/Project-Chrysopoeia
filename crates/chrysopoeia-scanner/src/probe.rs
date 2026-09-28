//! Probing files with ffprobe and turning its JSON into [`ProbeInfo`].
//!
//! The parser works on untyped JSON on purpose: ffprobe output differs
//! between versions and containers (numbers as strings, optional fields,
//! mkvmerge statistics tags), and one odd field must never make a whole file
//! unprobeable.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use chrysopoeia_core::{HdrFormat, ProbeInfo, StreamInfo, StreamKind};
use serde_json::{Map, Value};
use tokio::process::Command;

use crate::ProbeError;

/// Highest frame rate we believe. ffprobe reports timebase-derived rates
/// such as `90000/1` for cover art and some transport streams.
const MAX_FRAME_RATE: f64 = 1000.0;

/// Longest ffprobe failure reason shown to users.
const MAX_REASON_CHARS: usize = 200;

/// Implementation of [`crate::probe_file`].
pub(crate) async fn probe(
    ffprobe: &Path,
    path: &Path,
    timeout: Duration,
) -> Result<ProbeInfo, ProbeError> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|error| ProbeError::Unreadable(describe_open_error(&error)))?;
    if metadata.is_dir() {
        return Err(ProbeError::Unreadable(
            "This is a folder, not a media file".to_string(),
        ));
    }
    if !metadata.is_file() {
        return Err(ProbeError::Unreadable(
            "This is not a regular file (it may be a device, socket or pipe)".to_string(),
        ));
    }
    let size_bytes = metadata.len();
    if size_bytes == 0 {
        return Err(ProbeError::Unreadable("The file is empty".to_string()));
    }

    let input = input_argument(path);
    let child = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "-show_chapters",
            "-i",
        ])
        .arg(&input)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| ProbeError::Spawn(describe_spawn_error(ffprobe, &error)))?;

    // On timeout the future (and with it the child) is dropped, which kills
    // ffprobe thanks to `kill_on_drop`.
    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err(ProbeError::Spawn(format!(
                "ffprobe stopped unexpectedly ({error})"
            )));
        }
        Err(_) => return Err(ProbeError::Timeout(timeout)),
    };

    if !output.status.success() {
        let reason = describe_ffprobe_failure(&output.stderr, &input, output.status);
        tracing::debug!(
            path = %path.display(),
            stderr = %String::from_utf8_lossy(&output.stderr),
            "ffprobe rejected the file"
        );
        return Err(ProbeError::Unreadable(reason));
    }
    if !output.stderr.is_empty() {
        tracing::debug!(
            path = %path.display(),
            stderr = %String::from_utf8_lossy(&output.stderr),
            "ffprobe reported problems but read the file"
        );
    }
    parse(&output.stdout, size_bytes)
}

/// The path as an ffprobe input argument. Relative paths get a `./` prefix
/// so names like `Movie: Part 2.mkv` are not mistaken for a URL protocol.
fn input_argument(path: &Path) -> PathBuf {
    if path.is_relative() {
        Path::new(".").join(path)
    } else {
        path.to_path_buf()
    }
}

fn describe_open_error(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "The file no longer exists".to_string(),
        io::ErrorKind::PermissionDenied => {
            "Chrysopoeia does not have permission to read this file".to_string()
        }
        _ => format!("The file could not be opened ({error})"),
    }
}

fn describe_spawn_error(ffprobe: &Path, error: &io::Error) -> String {
    let shown = ffprobe.display();
    match error.kind() {
        io::ErrorKind::NotFound => format!(
            "ffprobe was not found at \"{shown}\". Install ffmpeg (it includes ffprobe) or set \
             FFPROBE_PATH to its location"
        ),
        io::ErrorKind::PermissionDenied => {
            format!("\"{shown}\" is not executable; check the permissions of the ffprobe program")
        }
        _ => format!("\"{shown}\" could not be started ({error})"),
    }
}

/// Turn ffprobe's stderr into one plain-language reason.
fn describe_ffprobe_failure(stderr: &[u8], input: &Path, status: ExitStatus) -> String {
    // Checked in order: the first match wins, so specific causes come first.
    const KNOWN: &[(&str, &str)] = &[
        (
            "permission denied",
            "Chrysopoeia does not have permission to read this file",
        ),
        (
            "operation not permitted",
            "Chrysopoeia is not allowed to read this file",
        ),
        ("no such file or directory", "The file no longer exists"),
        ("is a directory", "This is a folder, not a media file"),
        (
            "input/output error",
            "The disk reported a read error for this file (it may be damaged)",
        ),
        (
            "moov atom not found",
            "Not a readable media file (the MP4 index is missing, so the file is probably \
             incomplete)",
        ),
        (
            "ebml header parsing failed",
            "Not a readable media file (the Matroska header is damaged)",
        ),
        (
            "invalid data found when processing input",
            "Not a readable media file (the data is invalid or truncated)",
        ),
        (
            "end of file",
            "Not a readable media file (it ends too early; the copy may be incomplete)",
        ),
        (
            "cannot allocate memory",
            "ffprobe ran out of memory while reading this file",
        ),
    ];

    let text = String::from_utf8_lossy(stderr);
    let lower = text.to_lowercase();
    if let Some((_, reason)) = KNOWN.iter().find(|(needle, _)| lower.contains(needle)) {
        return (*reason).to_string();
    }

    let input = input.to_string_lossy();
    let detail = text
        .lines()
        .rev()
        .map(|line| clean_ffprobe_line(line, &input))
        .find(|line| !line.is_empty());
    match (detail, status.code()) {
        (Some(detail), _) => {
            let detail: String = detail.chars().take(MAX_REASON_CHARS).collect();
            format!("ffprobe could not read this file: {detail}")
        }
        (None, Some(code)) => format!("ffprobe could not read this file (exit code {code})"),
        (None, None) => "ffprobe stopped unexpectedly while reading this file".to_string(),
    }
}

/// Strip the `[demuxer @ 0x…] ` and `<input path>: ` prefixes from a line.
fn clean_ffprobe_line<'a>(line: &'a str, input: &str) -> &'a str {
    let mut line = line.trim();
    if line.starts_with('[') {
        if let Some(end) = line.find("] ") {
            line = line[end + 2..].trim_start();
        }
    }
    if let Some(rest) = line.strip_prefix(input) {
        line = rest.trim_start_matches(':').trim_start();
    }
    line.trim()
}

/// Implementation of [`crate::parse_ffprobe_json`].
pub(crate) fn parse(json: &[u8], size_bytes: u64) -> Result<ProbeInfo, ProbeError> {
    let root: Value = serde_json::from_slice(json)
        .map_err(|error| ProbeError::Parse(format!("the output is not valid JSON ({error})")))?;
    if !root.is_object() {
        return Err(ProbeError::Parse(
            "the output is not a JSON object".to_string(),
        ));
    }

    let format = root.get("format").filter(|f| f.is_object());
    let raw_streams: &[Value] = root
        .get("streams")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let streams = raw_streams
        .iter()
        .enumerate()
        .filter_map(|(position, raw)| parse_stream(position, raw))
        .collect();

    let container = format
        .and_then(|f| str_field(f, "format_name"))
        .and_then(|name| name.split(',').map(str::trim).find(|t| !t.is_empty()))
        .unwrap_or("unknown")
        .to_string();

    let duration_secs = format
        .and_then(|f| positive_f64(f.get("duration")))
        .or_else(|| max_positive(raw_streams.iter().map(|s| positive_f64(s.get("duration")))))
        .or_else(|| {
            max_positive(
                raw_streams
                    .iter()
                    .map(|s| statistics_tag(s, "DURATION").and_then(parse_clock_duration)),
            )
        })
        .or_else(|| {
            format.and_then(|f| statistics_tag(f, "DURATION").and_then(parse_clock_duration))
        });

    let chapters = root
        .get("chapters")
        .and_then(Value::as_array)
        .map_or(0, |c| u32::try_from(c.len()).unwrap_or(u32::MAX));

    Ok(ProbeInfo {
        container,
        format_long_name: format.and_then(|f| str_field(f, "format_long_name").map(String::from)),
        duration_secs,
        bit_rate: format
            .and_then(|f| u64_value(f.get("bit_rate")))
            .filter(|b| *b > 0),
        size_bytes,
        start_time: format.and_then(|f| f64_value(f.get("start_time"))),
        chapters,
        streams,
    })
}

fn parse_stream(position: usize, raw: &Value) -> Option<StreamInfo> {
    raw.as_object()?;
    let index = u64_value(raw.get("index"))
        .and_then(|i| u32::try_from(i).ok())
        .unwrap_or_else(|| u32::try_from(position).unwrap_or(u32::MAX));
    let kind = str_field(raw, "codec_type").and_then(|t| match t.to_ascii_lowercase().as_str() {
        "video" => Some(StreamKind::Video),
        "audio" => Some(StreamKind::Audio),
        "subtitle" => Some(StreamKind::Subtitle),
        "attachment" => Some(StreamKind::Attachment),
        "data" => Some(StreamKind::Data),
        _ => None,
    });
    let codec_tag = str_field(raw, "codec_tag_string").filter(|tag| is_printable_tag(tag));
    let codec = str_field(raw, "codec_name")
        .map(str::to_string)
        .or_else(|| codec_tag.map(str::to_ascii_lowercase))
        .unwrap_or_else(|| "unknown".to_string());
    let disposition = raw.get("disposition");

    let mut stream = StreamInfo {
        index,
        kind,
        codec,
        profile: known_str(raw, "profile"),
        language: tag(raw, "language").map(str::to_ascii_lowercase),
        title: tag(raw, "title").map(String::from),
        is_default: flag(disposition, "default"),
        is_forced: flag(disposition, "forced"),
        is_attached_pic: flag(disposition, "attached_pic"),
        bit_rate: u64_value(raw.get("bit_rate"))
            .filter(|b| *b > 0)
            .or_else(|| statistics_tag(raw, "BPS").and_then(parse_u64_str))
            .filter(|b| *b > 0),
        ..StreamInfo::default()
    };

    match kind {
        Some(StreamKind::Video) => {
            stream.width = positive_u32(raw.get("width"));
            stream.height = positive_u32(raw.get("height"));
            stream.pix_fmt = known_str(raw, "pix_fmt");
            stream.bit_depth = u64_value(raw.get("bits_per_raw_sample"))
                .filter(|d| (1..=32).contains(d))
                .and_then(|d| u8::try_from(d).ok())
                .or_else(|| stream.pix_fmt.as_deref().and_then(bit_depth_from_pix_fmt));
            stream.frame_rate = frame_rate(raw.get("avg_frame_rate"))
                .or_else(|| frame_rate(raw.get("r_frame_rate")));
            stream.color_primaries = known_str(raw, "color_primaries");
            stream.color_transfer = known_str(raw, "color_transfer");
            stream.color_space = known_str(raw, "color_space");
            stream.color_range = known_str(raw, "color_range");
            stream.hdr = detect_hdr(raw, stream.color_transfer.as_deref(), codec_tag);
            stream.interlaced = str_field(raw, "field_order").is_some_and(|order| {
                matches!(
                    order.to_ascii_lowercase().as_str(),
                    "tt" | "bb" | "tb" | "bt"
                )
            });
        }
        Some(StreamKind::Audio) => {
            stream.channels = positive_u32(raw.get("channels"));
            stream.channel_layout = known_str(raw, "channel_layout");
            stream.sample_rate = positive_u32(raw.get("sample_rate"));
        }
        _ => {}
    }
    Some(stream)
}

/// HDR signalling of a video stream. Dolby Vision and HDR10+ are recognised
/// from side data (or Dolby Vision sample entries), HDR10 and HLG from the
/// transfer characteristics.
fn detect_hdr(raw: &Value, transfer: Option<&str>, codec_tag: Option<&str>) -> Option<HdrFormat> {
    let side_data_types: Vec<String> = raw
        .get("side_data_list")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|entry| str_field(entry, "side_data_type"))
                .map(str::to_ascii_lowercase)
                .collect()
        })
        .unwrap_or_default();
    let has_side_data = |needle: &str| side_data_types.iter().any(|t| t.contains(needle));

    let dolby_vision_tag = codec_tag.is_some_and(|tag| {
        ["dvhe", "dvh1", "dva1", "dvav", "dav1"]
            .iter()
            .any(|dv| dv.eq_ignore_ascii_case(tag))
    });
    if has_side_data("dovi") || has_side_data("dolby vision") || dolby_vision_tag {
        return Some(HdrFormat::DolbyVision);
    }
    if has_side_data("smpte2094-40") || has_side_data("hdr10+") {
        return Some(HdrFormat::Hdr10Plus);
    }
    match transfer.map(str::to_ascii_lowercase).as_deref() {
        Some("smpte2084") => Some(HdrFormat::Hdr10),
        Some("arib-std-b67") => Some(HdrFormat::Hlg),
        _ => None,
    }
}

/// Bit depth implied by an ffmpeg pixel format name, when it is one we know.
fn bit_depth_from_pix_fmt(pix_fmt: &str) -> Option<u8> {
    /// Common pixel formats with 8 bits per component.
    const EIGHT_BIT: &[&str] = &[
        "yuv420p",
        "yuvj420p",
        "yuv422p",
        "yuvj422p",
        "yuv444p",
        "yuvj444p",
        "yuv440p",
        "yuvj440p",
        "yuv411p",
        "yuvj411p",
        "yuv410p",
        "yuva420p",
        "yuva422p",
        "yuva444p",
        "nv12",
        "nv21",
        "nv16",
        "nv24",
        "nv42",
        "yuyv422",
        "uyvy422",
        "yvyu422",
        "uyyvyy411",
        "gray",
        "ya8",
        "rgb24",
        "bgr24",
        "rgba",
        "bgra",
        "argb",
        "abgr",
        "rgb0",
        "bgr0",
        "0rgb",
        "0bgr",
        "gbrp",
        "gbrap",
        "pal8",
        "vuya",
        "vuyx",
        "ayuv",
    ];

    let name = pix_fmt.trim().to_ascii_lowercase();
    let base = name
        .strip_suffix("le")
        .or_else(|| name.strip_suffix("be"))
        .unwrap_or(&name);

    // Semi-planar formats: p010, p012, p016, p210, p410, ...
    if let Some(digits) = base.strip_prefix('p') {
        if digits.len() == 3 && digits.bytes().all(|b| b.is_ascii_digit()) {
            return digits[1..].parse().ok();
        }
    }
    match base {
        "y210" | "xv30" | "v30x" | "x2rgb10" | "x2bgr10" | "nv20" => return Some(10),
        "y212" | "xv36" => return Some(12),
        "rgb48" | "bgr48" | "rgba64" | "bgra64" | "ayuv64" | "y216" => return Some(16),
        _ => {}
    }
    // Planar formats with an explicit depth: yuv420p10, yuva444p16, gbrp12, gray10.
    let stem = base.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = &base[stem.len()..];
    let planar = stem == "gray"
        || (stem.ends_with('p') && (stem.starts_with("yuv") || stem.starts_with("gbr")));
    if planar && !digits.is_empty() {
        if let Ok(depth) = digits.parse::<u8>() {
            if (8..=16).contains(&depth) {
                return Some(depth);
            }
        }
    }
    EIGHT_BIT.contains(&base).then_some(8)
}

/// Parse an ffprobe rational like `24000/1001` (or a plain number) into a
/// frame rate, ignoring `0/0` and implausible values.
fn frame_rate(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    let Some(text) = value.as_str().map(str::trim) else {
        return f64_value(Some(value)).filter(|r| *r > 0.0 && *r <= MAX_FRAME_RATE);
    };
    let rate = match text.split_once('/') {
        Some((num, den)) => {
            let num: f64 = num.trim().parse().ok()?;
            let den: f64 = den.trim().parse().ok()?;
            if den == 0.0 {
                return None;
            }
            num / den
        }
        None => text.parse().ok()?,
    };
    (rate.is_finite() && rate > 0.0 && rate <= MAX_FRAME_RATE).then_some(rate)
}

/// Parse a clock duration such as mkvmerge's `01:23:45.678000000` (also
/// `MM:SS.s` and plain seconds).
fn parse_clock_duration(text: &str) -> Option<f64> {
    let parts: Vec<&str> = text.trim().split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let (seconds, larger_units) = parts.split_last()?;
    let mut total: f64 = seconds.trim().parse().ok()?;
    for (part, unit_secs) in larger_units.iter().rev().zip([60.0, 3600.0]) {
        let value: u64 = part.trim().parse().ok()?;
        total += value as f64 * unit_secs;
    }
    (total.is_finite() && total > 0.0).then_some(total)
}

/// Whether a `codec_tag_string` is a real FourCC (ffprobe prints unprintable
/// bytes as `[0]`).
fn is_printable_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
}

/// A non-empty string field.
fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// A string field, leaving out ffprobe's placeholders for "not known".
fn known_str(value: &Value, key: &str) -> Option<String> {
    str_field(value, key)
        .filter(|s| {
            !["unknown", "unspecified", "reserved", "none", "n/a"]
                .iter()
                .any(|placeholder| s.eq_ignore_ascii_case(placeholder))
        })
        .map(String::from)
}

fn tags(value: &Value) -> Option<&Map<String, Value>> {
    value.get("tags").and_then(Value::as_object)
}

/// A tag value, matching the key ignoring case.
fn tag<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    tags(value)?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .and_then(|(_, v)| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// An mkvmerge statistics tag (`BPS`, `DURATION`, ...), which older mkvmerge
/// versions write with a language suffix (`BPS-eng`).
fn statistics_tag<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    tag(value, key).or_else(|| {
        tags(value)?
            .iter()
            .find(|(k, _)| {
                k.len() > key.len()
                    && k.is_char_boundary(key.len())
                    && k[..key.len()].eq_ignore_ascii_case(key)
                    && k[key.len()..].starts_with('-')
            })
            .and_then(|(_, v)| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    })
}

/// A disposition flag (`1`, `true` or `"1"`).
fn flag(disposition: Option<&Value>, key: &str) -> bool {
    match disposition.and_then(|d| d.get(key)) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0),
        Some(Value::String(s)) => matches!(s.trim(), "1" | "true"),
        _ => false,
    }
}

/// A finite number, whether ffprobe wrote it as a number or a string.
fn f64_value(value: Option<&Value>) -> Option<f64> {
    let number = match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    };
    number.filter(|v: &f64| v.is_finite())
}

fn positive_f64(value: Option<&Value>) -> Option<f64> {
    f64_value(value).filter(|v| *v > 0.0)
}

fn max_positive(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    values.flatten().filter(|v| *v > 0.0).reduce(f64::max)
}

/// A non-negative integer, whether written as a number or a string.
fn u64_value(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(n) => n.as_u64().or_else(|| float_to_u64(n.as_f64()?)),
        Value::String(s) => parse_u64_str(s),
        _ => None,
    }
}

fn parse_u64_str(text: &str) -> Option<u64> {
    let text = text.trim();
    text.parse()
        .ok()
        .or_else(|| float_to_u64(text.parse().ok()?))
}

fn float_to_u64(value: f64) -> Option<u64> {
    (value.is_finite() && value >= 0.0 && value < u64::MAX as f64).then_some(value as u64)
}

fn positive_u32(value: Option<&Value>) -> Option<u32> {
    u64_value(value)
        .filter(|v| *v > 0)
        .and_then(|v| u32::try_from(v).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| (a - b).abs() < 1e-3)
    }

    #[test]
    fn mkv_with_side_layout_ac3_and_srt() {
        let probe = parse(&fixture("mkv_ac3_51_side_srt.json"), 1_692_412).unwrap();
        assert_eq!(probe.container, "matroska");
        assert_eq!(probe.format_long_name.as_deref(), Some("Matroska / WebM"));
        assert!(close(probe.duration_secs, 5.0));
        assert_eq!(probe.bit_rate, Some(2_707_859));
        assert_eq!(probe.size_bytes, 1_692_412);
        assert!(close(probe.start_time, -0.006));
        assert_eq!(probe.chapters, 0);
        assert_eq!(probe.streams.len(), 3);

        let video = probe.primary_video().unwrap();
        assert_eq!(video.index, 0);
        assert_eq!(video.codec, "h264");
        assert_eq!(video.profile.as_deref(), Some("High"));
        assert_eq!((video.width, video.height), (Some(1920), Some(1080)));
        assert_eq!(video.pix_fmt.as_deref(), Some("yuv420p"));
        assert_eq!(video.bit_depth, Some(8));
        assert!(close(video.frame_rate, 24.0));
        assert!(!video.interlaced);
        assert_eq!(video.hdr, None);
        assert_eq!(video.color_transfer, None);
        assert_eq!(probe.resolution_label(), Some("1080p"));

        let audio = &probe.streams[1];
        assert_eq!(audio.kind, Some(StreamKind::Audio));
        assert_eq!(audio.codec, "ac3");
        assert_eq!(audio.channels, Some(6));
        assert_eq!(audio.channel_layout.as_deref(), Some("5.1(side)"));
        assert_eq!(audio.sample_rate, Some(44_100));
        assert_eq!(audio.bit_rate, Some(384_000));
        assert_eq!(audio.bit_depth, None);
        assert_eq!(audio.frame_rate, None);

        let subtitle = &probe.streams[2];
        assert_eq!(subtitle.kind, Some(StreamKind::Subtitle));
        assert_eq!(subtitle.codec, "subrip");
        assert_eq!(subtitle.language.as_deref(), Some("eng"));
        assert!(!subtitle.is_default && !subtitle.is_forced);
    }

    #[test]
    fn hdr10_hevc_with_mastering_metadata() {
        let probe = parse(&fixture("hevc_hdr10_mastering.json"), 60_549_887_123).unwrap();
        assert_eq!(probe.container, "matroska");
        assert!(close(probe.duration_secs, 7726.72));
        assert_eq!(probe.bit_rate, Some(62_691_534));
        assert_eq!(probe.chapters, 3);
        assert_eq!(probe.hdr(), Some(HdrFormat::Hdr10));
        assert_eq!(probe.resolution_label(), Some("4K"));

        let video = probe.primary_video().unwrap();
        assert_eq!(video.codec, "hevc");
        assert_eq!(video.profile.as_deref(), Some("Main 10"));
        assert_eq!(video.bit_depth, Some(10));
        assert!(close(video.frame_rate, 24000.0 / 1001.0));
        assert_eq!(video.color_primaries.as_deref(), Some("bt2020"));
        assert_eq!(video.color_transfer.as_deref(), Some("smpte2084"));
        assert_eq!(video.color_space.as_deref(), Some("bt2020nc"));
        assert_eq!(video.color_range.as_deref(), Some("tv"));
        assert_eq!(video.title.as_deref(), Some("Main feature"));
        assert!(video.is_default);
        // No stream bit_rate: taken from the mkvmerge BPS tag.
        assert_eq!(video.bit_rate, Some(58_234_221));

        let truehd = &probe.streams[1];
        assert_eq!(truehd.codec, "truehd");
        assert_eq!(truehd.channels, Some(8));
        assert_eq!(truehd.channel_layout.as_deref(), Some("7.1"));
        assert_eq!(truehd.sample_rate, Some(48_000));
        assert_eq!(truehd.title.as_deref(), Some("TrueHD Atmos 7.1"));

        let pgs = &probe.streams[2];
        assert_eq!(pgs.codec, "hdmv_pgs_subtitle");
        assert!(pgs.is_forced);
        // Subtitle dimensions are not video properties.
        assert_eq!(pgs.width, None);
    }

    #[test]
    fn dolby_vision_profile_8() {
        let probe = parse(&fixture("hevc_dolby_vision_p8.json"), 14_471_237_490).unwrap();
        assert_eq!(probe.container, "mov");
        assert_eq!(probe.format_long_name.as_deref(), Some("QuickTime / MOV"));
        assert_eq!(probe.hdr(), Some(HdrFormat::DolbyVision));
        assert_eq!(probe.streams.len(), 4);

        let video = probe.primary_video().unwrap();
        assert_eq!(video.bit_rate, Some(16_804_391));
        assert_eq!(video.language.as_deref(), Some("und"));
        assert_eq!((video.width, video.height), (Some(3840), Some(1608)));
        assert_eq!(probe.resolution_label(), Some("4K"));

        let eac3 = &probe.streams[1];
        assert_eq!(
            eac3.profile.as_deref(),
            Some("Dolby Digital Plus + Dolby Atmos")
        );
        assert_eq!(eac3.language.as_deref(), Some("eng"));
        // Tag keys are matched ignoring case ("Language").
        assert_eq!(probe.streams[2].language.as_deref(), Some("spa"));
        assert_eq!(probe.streams[2].codec, "mov_text");

        // A timecode track has no codec_name: fall back to the FourCC.
        let timecode = &probe.streams[3];
        assert_eq!(timecode.kind, Some(StreamKind::Data));
        assert_eq!(timecode.codec, "tmcd");
    }

    #[test]
    fn mp3_with_cover_art_is_audio_only() {
        let probe = parse(&fixture("mp3_cover_art.json"), 105_022).unwrap();
        assert_eq!(probe.container, "mp3");
        assert!(probe.is_audio_only());
        assert_eq!(probe.video_codec(), None);
        assert_eq!(probe.audio_codec(), Some("mp3"));
        assert!(close(probe.duration_secs, 3.030204));

        let cover = &probe.streams[1];
        assert_eq!(cover.kind, Some(StreamKind::Video));
        assert!(cover.is_attached_pic);
        assert_eq!(cover.codec, "png");
        assert_eq!(cover.title.as_deref(), Some("Album cover"));
        assert_eq!(cover.bit_depth, Some(8));
        // avg_frame_rate is 0/0 and r_frame_rate is the 90 kHz timebase.
        assert_eq!(cover.frame_rate, None);
        assert_eq!(cover.color_range.as_deref(), Some("pc"));
    }

    #[test]
    fn interlaced_mpeg2_transport_stream() {
        let probe = parse(&fixture("mpegts_interlaced_mpeg2.json"), 1_000_000).unwrap();
        assert_eq!(probe.container, "mpegts");
        let video = probe.primary_video().unwrap();
        assert_eq!(video.codec, "mpeg2video");
        assert!(video.interlaced);
        assert_eq!(video.bit_depth, Some(8));
        assert_eq!((video.width, video.height), (Some(720), Some(576)));
        assert_eq!(probe.resolution_label(), Some("576p"));
        assert_eq!(probe.audio_codec(), Some("mp2"));
        assert!(probe.duration_secs.is_some_and(|d| d > 2.0));
    }

    #[test]
    fn mkvmerge_statistics_tags_without_format_duration() {
        let probe = parse(&fixture("mkvmerge_bps_duration_tags.json"), 11_234_567_890).unwrap();
        // No format or stream duration: the longest DURATION tag wins.
        assert!(close(probe.duration_secs, 6723.68));
        assert_eq!(probe.bit_rate, None);
        assert_eq!(probe.chapters, 2);

        let video = probe.primary_video().unwrap();
        assert_eq!(video.bit_rate, Some(9_512_345));
        // avg_frame_rate is 0/0: fall back to r_frame_rate.
        assert!(close(video.frame_rate, 24000.0 / 1001.0));
        assert_eq!(video.color_transfer.as_deref(), Some("bt709"));
        assert_eq!(video.hdr, None);
        assert_eq!(probe.resolution_label(), Some("1080p"));

        let dts = &probe.streams[1];
        assert_eq!(dts.bit_rate, Some(3_456_789));
        assert_eq!(dts.language.as_deref(), Some("eng"));
        assert_eq!(dts.title.as_deref(), Some("Surround 5.1"));
        assert_eq!(dts.profile.as_deref(), Some("DTS-HD MA"));

        let commentary = &probe.streams[2];
        assert_eq!(commentary.bit_rate, Some(192_000));
        assert_eq!(commentary.language.as_deref(), Some("fre"));

        let font = &probe.streams[4];
        assert_eq!(font.kind, Some(StreamKind::Attachment));
        assert_eq!(font.codec, "ttf");
    }

    #[test]
    fn garbage_is_a_parse_error() {
        for garbage in [
            &b"this is not json"[..],
            b"",
            b"[1, 2, 3]",
            b"\"text\"",
            b"{\"streams\": [",
        ] {
            let error = parse(garbage, 1).unwrap_err();
            assert!(matches!(error, ProbeError::Parse(_)), "{error:?}");
        }
    }

    #[test]
    fn tolerates_missing_extra_and_odd_fields() {
        let probe = parse(br#"{}"#, 5).unwrap();
        assert_eq!(probe.container, "unknown");
        assert!(probe.streams.is_empty());
        assert_eq!(probe.duration_secs, None);

        let json = br#"{
            "streams": [
                "not an object",
                {"codec_type": "video", "codec_name": "h264", "width": "1280", "height": 720.0,
                 "avg_frame_rate": "25", "bits_per_raw_sample": "N/A", "pix_fmt": "yuv420p10le",
                 "duration": "12.5", "disposition": {"default": "1", "forced": true},
                 "color_primaries": "unknown", "field_order": "BB", "new_field": [1, 2]},
                {"index": 7, "codec_type": "whatever", "codec_tag_string": "[0][0][0][0]"},
                {"index": "8", "codec_type": "audio", "codec_name": "opus",
                 "channels": "2", "sample_rate": 48000, "bit_rate": "N/A",
                 "tags": {"LANGUAGE": " JPN "}}
            ],
            "format": {"format_name": "matroska,webm", "duration": "N/A", "bit_rate": 1234.9,
                       "start_time": "N/A", "extra": {"nested": true}},
            "chapters": {"not": "an array"}
        }"#;
        let probe = parse(json, 5).unwrap();
        assert_eq!(probe.streams.len(), 3);
        assert!(close(probe.duration_secs, 12.5));
        assert_eq!(probe.bit_rate, Some(1234));
        assert_eq!(probe.start_time, None);
        assert_eq!(probe.chapters, 0);

        let video = &probe.streams[0];
        assert_eq!(video.index, 1, "falls back to the array position");
        assert_eq!((video.width, video.height), (Some(1280), Some(720)));
        assert!(close(video.frame_rate, 25.0));
        assert_eq!(video.bit_depth, Some(10));
        assert!(video.is_default && video.is_forced);
        assert_eq!(video.color_primaries, None);
        assert!(video.interlaced);

        let unknown = &probe.streams[1];
        assert_eq!(unknown.index, 7);
        assert_eq!(unknown.kind, None);
        assert_eq!(unknown.codec, "unknown");

        let audio = &probe.streams[2];
        assert_eq!(audio.index, 8);
        assert_eq!(audio.channels, Some(2));
        assert_eq!(audio.sample_rate, Some(48_000));
        assert_eq!(audio.bit_rate, None);
        assert_eq!(audio.language.as_deref(), Some("jpn"));
    }

    #[test]
    fn hdr_variants() {
        let stream = |extra: &str| {
            let json = format!(
                r#"{{"streams": [{{"codec_type": "video", "codec_name": "hevc" {extra}}}]}}"#
            );
            parse(json.as_bytes(), 1).unwrap().hdr()
        };
        assert_eq!(
            stream(r#", "color_transfer": "arib-std-b67""#),
            Some(HdrFormat::Hlg)
        );
        assert_eq!(
            stream(r#", "color_transfer": "smpte2084""#),
            Some(HdrFormat::Hdr10)
        );
        assert_eq!(
            stream(
                r#", "color_transfer": "smpte2084", "side_data_list": [
                    {"side_data_type": "HDR Dynamic Metadata SMPTE2094-40 (HDR10+)"}]"#
            ),
            Some(HdrFormat::Hdr10Plus)
        );
        assert_eq!(
            stream(r#", "color_transfer": "arib-std-b67", "codec_tag_string": "dvh1""#),
            Some(HdrFormat::DolbyVision)
        );
        assert_eq!(stream(r#", "color_transfer": "bt709""#), None);
        assert_eq!(stream(""), None);
    }

    #[test]
    fn pixel_format_bit_depths() {
        for (fmt, depth) in [
            ("yuv420p", Some(8)),
            ("yuvj420p", Some(8)),
            ("nv12", Some(8)),
            ("rgb24", Some(8)),
            ("yuv420p10le", Some(10)),
            ("yuv420p10be", Some(10)),
            ("yuv422p12le", Some(12)),
            ("yuva444p16le", Some(16)),
            ("gbrp10le", Some(10)),
            ("gray10le", Some(10)),
            ("p010le", Some(10)),
            ("p016le", Some(16)),
            ("p210le", Some(10)),
            ("nv20le", Some(10)),
            ("x2rgb10le", Some(10)),
            ("rgb48le", Some(16)),
            ("xv36le", Some(12)),
            ("bayer_rggb8", None),
            ("mystery", None),
        ] {
            assert_eq!(bit_depth_from_pix_fmt(fmt), depth, "{fmt}");
        }
    }

    #[test]
    fn clock_durations_and_rates() {
        assert!(close(parse_clock_duration("01:23:45.678000000"), 5025.678));
        assert!(close(parse_clock_duration("00:00:02.000000000"), 2.0));
        assert!(close(parse_clock_duration("2:03.5"), 123.5));
        assert!(close(parse_clock_duration("42.1"), 42.1));
        assert_eq!(parse_clock_duration("00:00:00.000000000"), None);
        assert_eq!(parse_clock_duration("garbage"), None);
        assert_eq!(parse_clock_duration("1:2:3:4:5"), None);

        let rate = |s: &str| frame_rate(Some(&Value::String(s.to_string())));
        assert!(close(rate("30000/1001"), 29.97));
        assert_eq!(rate("0/0"), None);
        assert_eq!(rate("90000/1"), None);
        assert_eq!(rate("x/y"), None);
    }

    #[test]
    fn statistics_tags_prefer_the_plain_key() {
        let value: Value = serde_json::from_str(
            r#"{"tags": {"BPS-eng": "1", "bps": "2", "BPSX": "3", "DURATION-fre": "00:00:01"}}"#,
        )
        .unwrap();
        assert_eq!(statistics_tag(&value, "BPS"), Some("2"));
        assert_eq!(statistics_tag(&value, "duration"), Some("00:00:01"));
        assert_eq!(statistics_tag(&value, "NUMBER_OF_FRAMES"), None);
    }

    #[cfg(unix)]
    fn exit_status(code: i32) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(code << 8)
    }

    #[cfg(unix)]
    #[test]
    fn ffprobe_failures_become_plain_reasons() {
        let input = Path::new("/media/Broken/Fake.mp4");
        let fake = b"[mov,mp4,m4a,3gp,3g2,mj2 @ 0x5573] moov atom not found\n\
                     /media/Broken/Fake.mp4: Invalid data found when processing input\n";
        assert!(
            describe_ffprobe_failure(fake, input, exit_status(1)).contains("MP4 index is missing")
        );
        let invalid = b"/media/Broken/Fake.mp4: Invalid data found when processing input\n";
        assert_eq!(
            describe_ffprobe_failure(invalid, input, exit_status(1)),
            "Not a readable media file (the data is invalid or truncated)"
        );
        let denied = b"/media/Broken/Fake.mp4: Permission denied\n";
        assert_eq!(
            describe_ffprobe_failure(denied, input, exit_status(1)),
            "Chrysopoeia does not have permission to read this file"
        );
        let missing = b"/media/Broken/Fake.mp4: No such file or directory\n";
        assert_eq!(
            describe_ffprobe_failure(missing, input, exit_status(1)),
            "The file no longer exists"
        );
        let other =
            b"[matroska,webm @ 0x1] Something odd\n/media/Broken/Fake.mp4: Weird failure\n\n";
        assert_eq!(
            describe_ffprobe_failure(other, input, exit_status(1)),
            "ffprobe could not read this file: Weird failure"
        );
        assert_eq!(
            describe_ffprobe_failure(b"", input, exit_status(3)),
            "ffprobe could not read this file (exit code 3)"
        );
    }

    fn tool_available(program: &str) -> bool {
        std::process::Command::new(program)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Generate the synthetic library with scripts/make-test-media.sh, or
    /// `None` (after saying why) when ffmpeg is not installed.
    fn generate_library(seconds: u32) -> Option<tempfile::TempDir> {
        if !tool_available("ffmpeg") || !tool_available("ffprobe") {
            eprintln!("skipping: ffmpeg/ffprobe not found on PATH");
            return None;
        }
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/make-test-media.sh");
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("sh")
            .arg(&script)
            .arg(dir.path())
            .arg(seconds.to_string())
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "make-test-media.sh failed");
        Some(dir)
    }

    #[tokio::test]
    async fn probes_every_generated_file() {
        let Some(library) = generate_library(2) else {
            return;
        };
        let root = library.path();
        let ffprobe = Path::new("ffprobe");
        let timeout = Duration::from_secs(60);

        // The walker finds exactly the media files (the script's hidden
        // leftovers are ignored by the default patterns).
        let opts = crate::ScanOptions {
            ignore_patterns: chrysopoeia_core::Settings::default().ignore_patterns,
            ..crate::ScanOptions::default()
        };
        let walked = crate::walk_library(root, &opts).unwrap();
        let names: Vec<String> = walked
            .files
            .iter()
            .map(|f| {
                f.path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            names,
            [
                "Broken/Fake.mp4",
                "Broken/Truncated.mkv",
                "Movies/Big Test (2020)/Big Test (2020).mp4",
                "Movies/Old Home Video.avi",
                "Music/Tone.flac",
                "TV/Show/Season 01/Show - S01E01.mkv",
                "TV/Show/Season 01/Show - S01E02.mkv",
                "TV/Show/Season 01/Show - S01E03.ts",
            ]
        );

        for file in &walked.files {
            let result = probe(ffprobe, &file.path, timeout).await;
            let name = file
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            match name.as_str() {
                "Big Test (2020).mp4" => {
                    let p = result.unwrap();
                    assert_eq!(p.container, "mov");
                    assert_eq!(p.size_bytes, file.size);
                    assert!(
                        p.duration_secs.is_some_and(|d| (d - 2.0).abs() < 0.2),
                        "{p:?}"
                    );
                    let v = p.primary_video().unwrap();
                    assert_eq!(
                        (v.codec.as_str(), v.width, v.height),
                        ("h264", Some(1280), Some(720))
                    );
                    assert_eq!(v.bit_depth, Some(8));
                    assert!(v.frame_rate.is_some_and(|r| (r - 24.0).abs() < 0.01));
                    let audio: Vec<_> = p.audio_streams().collect();
                    assert_eq!(audio.len(), 2);
                    assert_eq!(audio[0].codec, "aac");
                    assert_eq!(audio[0].language.as_deref(), Some("eng"));
                    assert_eq!(audio[1].language.as_deref(), Some("jpn"));
                    assert_eq!(audio[0].channels, Some(2));
                }
                "Show - S01E01.mkv" => {
                    let p = result.unwrap();
                    assert_eq!(p.container, "matroska");
                    assert_eq!(p.resolution_label(), Some("1080p"));
                    let ac3 = p.audio_streams().next().unwrap();
                    assert_eq!(ac3.codec, "ac3");
                    assert_eq!(ac3.channel_layout.as_deref(), Some("5.1(side)"));
                    let sub = p.subtitle_streams().next().unwrap();
                    assert_eq!(sub.codec, "subrip");
                    assert_eq!(sub.language.as_deref(), Some("eng"));
                }
                "Show - S01E02.mkv" => {
                    let p = result.unwrap();
                    let v = p.primary_video().unwrap();
                    assert_eq!(v.codec, "hevc");
                    assert_eq!(v.bit_depth, Some(10));
                    assert_eq!(v.pix_fmt.as_deref(), Some("yuv420p10le"));
                    assert_eq!(v.hdr, None);
                }
                "Show - S01E03.ts" => {
                    let p = result.unwrap();
                    assert_eq!(p.container, "mpegts");
                    let v = p.primary_video().unwrap();
                    assert_eq!(v.codec, "mpeg2video");
                    assert!(v.interlaced);
                    assert_eq!(p.audio_codec(), Some("mp2"));
                }
                "Old Home Video.avi" => {
                    let p = result.unwrap();
                    assert_eq!(p.container, "avi");
                    let v = p.primary_video().unwrap();
                    assert_eq!(v.codec, "mpeg4");
                    // The script asks for 639x359; ffmpeg's mpeg4 encoder
                    // rounds odd sizes down to even ones.
                    assert!(matches!(v.width, Some(638 | 639)), "{v:?}");
                    assert!(matches!(v.height, Some(358 | 359)), "{v:?}");
                    assert_eq!(p.audio_codec(), Some("mp3"));
                }
                "Tone.flac" => {
                    let p = result.unwrap();
                    assert_eq!(p.container, "flac");
                    assert!(p.is_audio_only());
                }
                "Truncated.mkv" => match result {
                    Ok(p) => assert!(p.primary_video().is_none_or(|v| v.codec == "h264")),
                    Err(ProbeError::Unreadable(reason)) => assert!(!reason.is_empty()),
                    Err(other) => panic!("unexpected error for a truncated file: {other:?}"),
                },
                "Fake.mp4" => match result {
                    Err(ProbeError::Unreadable(reason)) => {
                        assert!(reason.starts_with("Not a readable media file"), "{reason}");
                    }
                    other => panic!("a text file must not probe: {other:?}"),
                },
                other => panic!("unexpected file {other}"),
            }
        }
    }

    #[tokio::test]
    async fn files_that_are_not_there_or_not_files() {
        let dir = tempfile::tempdir().unwrap();
        let ffprobe = Path::new("ffprobe");
        let timeout = Duration::from_secs(5);
        let unreadable = |result: Result<ProbeInfo, ProbeError>| match result {
            Err(ProbeError::Unreadable(reason)) => reason,
            other => panic!("expected Unreadable, got {other:?}"),
        };

        let reason = unreadable(probe(ffprobe, &dir.path().join("gone.mkv"), timeout).await);
        assert_eq!(reason, "The file no longer exists");
        let reason = unreadable(probe(ffprobe, dir.path(), timeout).await);
        assert_eq!(reason, "This is a folder, not a media file");
        let empty = dir.path().join("empty.mkv");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(
            unreadable(probe(ffprobe, &empty, timeout).await),
            "The file is empty"
        );
    }

    /// Write an executable script to stand in for ffprobe.
    #[cfg(unix)]
    fn fake_ffprobe(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Probe with a freshly written script, retrying while another test
    /// thread's fork still holds it open for writing ("text file busy").
    #[cfg(unix)]
    async fn probe_with(
        script: &Path,
        file: &Path,
        timeout: Duration,
    ) -> Result<ProbeInfo, ProbeError> {
        for _ in 0..50 {
            match probe(script, file, timeout).await {
                Err(ProbeError::Spawn(message)) if message.contains("busy") => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => return other,
            }
        }
        panic!("the fake ffprobe stayed busy");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn slow_ffprobe_is_killed_after_the_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let script = fake_ffprobe(dir.path(), "ffprobe-hangs", "exec sleep 30");
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"data").unwrap();

        let started = std::time::Instant::now();
        let result = probe_with(&script, &media, Duration::from_millis(200)).await;
        assert!(
            matches!(result, Err(ProbeError::Timeout(t)) if t == Duration::from_millis(200)),
            "{result:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ffprobe_problems_are_explained() {
        let dir = tempfile::tempdir().unwrap();
        let media = dir.path().join("movie.mkv");
        std::fs::write(&media, b"data").unwrap();
        let timeout = Duration::from_secs(10);

        // Arguments: -v error -print_format json -show_format -show_streams
        // -show_chapters -i <path>, so the path is $9.
        let denied = fake_ffprobe(
            dir.path(),
            "denied",
            "echo \"$9: Permission denied\" >&2\nexit 1",
        );
        match probe_with(&denied, &media, timeout).await {
            Err(ProbeError::Unreadable(reason)) => {
                assert_eq!(
                    reason,
                    "Chrysopoeia does not have permission to read this file"
                );
            }
            other => panic!("{other:?}"),
        }

        let garbage = fake_ffprobe(dir.path(), "garbage", "echo 'not json at all'");
        assert!(matches!(
            probe_with(&garbage, &media, timeout).await,
            Err(ProbeError::Parse(_))
        ));

        let echo_path = fake_ffprobe(
            dir.path(),
            "echo-path",
            "printf '{\"format\":{\"format_name\":\"matroska,webm\",\"filename\":\"%s\"}}' \"$9\"",
        );
        let probed = probe_with(&echo_path, &media, timeout).await.unwrap();
        assert_eq!(probed.container, "matroska");
        assert_eq!(probed.size_bytes, 4);

        let missing = probe(&dir.path().join("no-ffprobe-here"), &media, timeout).await;
        match missing {
            Err(ProbeError::Spawn(message)) => {
                assert!(message.contains("was not found"), "{message}");
                assert!(message.contains("FFPROBE_PATH"), "{message}");
            }
            other => panic!("{other:?}"),
        }

        let not_executable = dir.path().join("not-executable");
        std::fs::write(&not_executable, b"#!/bin/sh\n").unwrap();
        match probe(&not_executable, &media, timeout).await {
            Err(ProbeError::Spawn(message)) => {
                assert!(message.contains("not executable"), "{message}")
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn relative_inputs_are_not_urls() {
        assert_eq!(
            input_argument(Path::new("Movie: Part 2.mkv")),
            Path::new("./Movie: Part 2.mkv")
        );
        assert_eq!(input_argument(Path::new("/m/a.mkv")), Path::new("/m/a.mkv"));
    }
}
