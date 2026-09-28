//! Helpers shared by the `plan_*` integration tests: stream builders, argument
//! inspection, a small ffprobe-to-`ProbeInfo` reader and synthetic media.
//!
//! The real prober lives in `chrysopoeia-scanner`; this reader exists so the
//! planner can be tested against real files independently of it.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use chrysopoeia_core::{
    AudioCodec, Container, EncoderCandidate, Goal, HdrFormat, ProbeInfo, QualityLevel, SpeedPreset,
    StreamInfo, StreamKind, SubtitlePolicy, TranscodeProfile, VideoCodec,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Builders

pub fn video(index: u32, codec: &str, width: u32, height: u32) -> StreamInfo {
    StreamInfo {
        index,
        kind: Some(StreamKind::Video),
        codec: codec.into(),
        width: Some(width),
        height: Some(height),
        pix_fmt: Some("yuv420p".into()),
        bit_depth: Some(8),
        frame_rate: Some(24.0),
        is_default: true,
        ..Default::default()
    }
}

pub fn video_10bit(index: u32, codec: &str, width: u32, height: u32) -> StreamInfo {
    StreamInfo {
        pix_fmt: Some("yuv420p10le".into()),
        bit_depth: None,
        ..video(index, codec, width, height)
    }
}

pub fn hdr10(mut v: StreamInfo) -> StreamInfo {
    v.color_primaries = Some("bt2020".into());
    v.color_transfer = Some("smpte2084".into());
    v.color_space = Some("bt2020nc".into());
    v.hdr = Some(HdrFormat::Hdr10);
    v
}

pub fn audio(index: u32, codec: &str, channels: u32, rate: u32, lang: Option<&str>) -> StreamInfo {
    StreamInfo {
        index,
        kind: Some(StreamKind::Audio),
        codec: codec.into(),
        channels: Some(channels),
        sample_rate: Some(rate),
        channel_layout: Some(
            match channels {
                1 => "mono",
                2 => "stereo",
                6 => "5.1(side)",
                8 => "7.1",
                _ => "unknown",
            }
            .into(),
        ),
        language: lang.map(Into::into),
        ..Default::default()
    }
}

pub fn subtitle(index: u32, codec: &str, lang: Option<&str>) -> StreamInfo {
    StreamInfo {
        index,
        kind: Some(StreamKind::Subtitle),
        codec: codec.into(),
        language: lang.map(Into::into),
        ..Default::default()
    }
}

pub fn attachment(index: u32) -> StreamInfo {
    StreamInfo {
        index,
        kind: Some(StreamKind::Attachment),
        codec: "ttf".into(),
        ..Default::default()
    }
}

pub fn probe_of(container: &str, streams: Vec<StreamInfo>) -> ProbeInfo {
    ProbeInfo {
        container: container.into(),
        duration_secs: Some(600.0),
        bit_rate: Some(8_000_000),
        size_bytes: 600_000_000,
        start_time: Some(0.0),
        streams,
        ..Default::default()
    }
}

pub fn profile(
    video_codec: VideoCodec,
    audio_codec: AudioCodec,
    container: Container,
) -> TranscodeProfile {
    TranscodeProfile {
        goal: Goal::Custom,
        video_codec,
        audio_codec,
        container,
        quality: QualityLevel::Balanced,
        speed: SpeedPreset::Fast,
        quality_override: None,
        max_height: None,
        subtitles: SubtitlePolicy::Keep,
        audio_languages: Vec::new(),
        subtitle_languages: Vec::new(),
        skip_efficient: false,
        min_savings_pct: None,
    }
}

/// Candidate for a registered encoder name.
pub fn candidate(name: &str, hw_decode: bool) -> EncoderCandidate {
    let info = chrysopoeia_core::encoder::find_encoder(name)
        .unwrap_or_else(|| panic!("{name} is not a registered encoder"));
    EncoderCandidate {
        name: name.into(),
        codec: info.codec,
        api: info.api,
        device: None,
        hw_decode,
    }
}

pub fn software(codec: VideoCodec) -> EncoderCandidate {
    let name = match codec {
        VideoCodec::Av1 => "libsvtav1",
        VideoCodec::Hevc => "libx265",
        VideoCodec::H264 => "libx264",
        VideoCodec::Vp9 => "libvpx-vp9",
    };
    candidate(name, false)
}

/// Build a plan for fixed dummy paths, panicking on error.
pub fn plan(
    probe: &ProbeInfo,
    profile: &TranscodeProfile,
    encoder: &EncoderCandidate,
) -> chrysopoeia_worker::FfmpegPlan {
    let ext = profile.container.extension();
    let output = PathBuf::from(format!("/tmp/out.{ext}"));
    chrysopoeia_worker::build_plan(&chrysopoeia_worker::PlanRequest {
        input: Path::new("/media/in.mkv"),
        output: &output,
        probe,
        profile,
        encoder,
    })
    .unwrap_or_else(|e| panic!("build_plan failed: {e:#}"))
}

// ---------------------------------------------------------------------------
// Argument inspection

/// Position of the first occurrence of `flag`.
pub fn pos(args: &[String], flag: &str) -> Option<usize> {
    args.iter().position(|a| a == flag)
}

/// Value following the first occurrence of `flag`.
pub fn value(args: &[String], flag: &str) -> Option<String> {
    pos(args, flag).and_then(|i| args.get(i + 1)).cloned()
}

/// Whether `flag value` appears as an adjacent pair anywhere.
pub fn has_pair(args: &[String], flag: &str, val: &str) -> bool {
    args.windows(2).any(|w| w[0] == flag && w[1] == val)
}

/// Index of the pair `flag value`.
pub fn pair_pos(args: &[String], flag: &str, val: &str) -> Option<usize> {
    args.windows(2).position(|w| w[0] == flag && w[1] == val)
}

/// All values of a repeated flag, in order (e.g. every `-map`).
pub fn values(args: &[String], flag: &str) -> Vec<String> {
    args.windows(2)
        .filter(|w| w[0] == flag)
        .map(|w| w[1].clone())
        .collect()
}

pub fn input_pos(args: &[String]) -> usize {
    pos(args, "-i").expect("plan has no -i")
}

/// Panics unless every listed flag appears before `-i`.
pub fn assert_before_input(args: &[String], flags: &[&str]) {
    let i = input_pos(args);
    for flag in flags {
        let p = pos(args, flag).unwrap_or_else(|| panic!("{flag} missing in {args:?}"));
        assert!(p < i, "{flag} must come before -i in {args:?}");
    }
}

pub fn assert_absent(args: &[String], flag: &str) {
    assert!(
        pos(args, flag).is_none(),
        "{flag} should be absent in {args:?}"
    );
}

pub fn vf(args: &[String]) -> Option<String> {
    value(args, "-vf")
}

// ---------------------------------------------------------------------------
// Tools

pub fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// True when ffmpeg and ffprobe both run; otherwise prints why the test is
/// skipped.
pub fn media_tools_available() -> bool {
    let ok = tool_available("ffmpeg") && tool_available("ffprobe");
    if !ok {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
    }
    ok
}

/// Encoder names compiled into this ffmpeg.
pub fn ffmpeg_encoders() -> &'static Vec<String> {
    static ENCODERS: OnceLock<Vec<String>> = OnceLock::new();
    ENCODERS.get_or_init(|| {
        let Ok(out) = Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .output()
        else {
            return Vec::new();
        };
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| {
                let mut parts = line.split_whitespace();
                let flags = parts.next()?;
                let name = parts.next()?;
                (flags.len() == 6 && flags != "------").then(|| name.to_string())
            })
            .collect()
    })
}

pub fn has_encoder(name: &str) -> bool {
    ffmpeg_encoders().iter().any(|e| e == name)
}

/// Synthetic library from `scripts/make-test-media.sh` (3 s clips), created
/// once per test process under Cargo's per-target temp directory. Returns
/// `None` (after printing why) when it cannot be generated.
pub fn media_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("plan-media");
        let _ = std::fs::remove_dir_all(&dir);
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/make-test-media.sh");
        let status = Command::new("sh").arg(&script).arg(&dir).arg("3").status();
        match status {
            Ok(s) if s.success() => {}
            other => {
                eprintln!("skipping: could not generate test media ({other:?})");
                return None;
            }
        }
        if let Err(what) = extra_samples(&dir) {
            eprintln!("skipping: could not generate the {what} sample");
            return None;
        }
        Some(dir)
    })
    .as_deref()
}

/// A minimal styled ASS script (bold text) for the subtitle samples.
const STYLED_ASS: &str = r"[Script Info]
ScriptType: v4.00+
PlayResX: 320
PlayResY: 240

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:00.50,0:00:02.00,Default,,0,0,0,,{\b1}Styled{\b0} line
";

/// Samples the script does not make. Returns the name of the one that failed.
fn extra_samples(dir: &Path) -> Result<(), &'static str> {
    let ffmpeg = |args: &[&str]| {
        Command::new("ffmpeg")
            .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
            .args(args)
            .current_dir(dir)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };

    // Odd-sized VP9 + Vorbis WebM: the script's AVI is rounded to even
    // dimensions by its MPEG-4 encoder, so odd sizes need their own file.
    let odd = ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=640x360:rate=25:duration=3,format=rgb24,scale=639:359,format=yuv420p",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=300:duration=3:sample_rate=44100",
        "-c:v",
        "libvpx-vp9",
        "-deadline",
        "realtime",
        "-cpu-used",
        "8",
        "-b:v",
        "1M",
        "-c:a",
        "libvorbis",
        "Odd Size.webm",
    ]);
    if !odd {
        return Err("odd-sized");
    }

    // HDR10-tagged 10-bit HEVC.
    let hdr = ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=640x360:rate=24:duration=3",
        "-c:v",
        "libx265",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p10le",
        "-x265-params",
        "log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc",
        "-color_primaries",
        "bt2020",
        "-color_trc",
        "smpte2084",
        "-colorspace",
        "bt2020nc",
        "HDR10.mkv",
    ]);
    if !hdr {
        return Err("HDR10");
    }

    // H.264 + AAC + styled ASS subtitle + font attachment in MKV.
    let wrote = std::fs::write(dir.join(".styled.ass"), STYLED_ASS).is_ok()
        && std::fs::write(dir.join(".font.ttf"), b"not really a font").is_ok();
    let styled = wrote
        && ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25:duration=3",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=400:duration=3",
            "-i",
            ".styled.ass",
            "-attach",
            ".font.ttf",
            "-metadata:s:t",
            "mimetype=application/x-truetype-font",
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-map",
            "2:s",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-c:s",
            "ass",
            "Styled.mkv",
        ]);
    if !styled {
        return Err("styled subtitle");
    }

    // H.264 + AAC MP4 with PNG cover art (a second, attached-picture video).
    // The picture comes from a one-frame image so it does not cut the clip.
    let cover = ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=red:size=100x100",
        "-frames:v",
        "1",
        ".cover.png",
    ]) && ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x240:rate=25:duration=3",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=500:duration=3",
        "-i",
        ".cover.png",
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-map",
        "2:v",
        "-c:v:0",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt:v:0",
        "yuv420p",
        "-c:v:1",
        "png",
        "-disposition:v:1",
        "attached_pic",
        "-c:a",
        "aac",
        "Cover.mp4",
    ]);
    if !cover {
        return Err("cover art");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// ffprobe → ProbeInfo (test-only)

/// Probe a file with ffprobe and map the JSON onto `ProbeInfo`, covering the
/// fields the planner reads.
pub fn probe_file(path: &Path) -> ProbeInfo {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "-show_chapters",
        ])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    assert!(
        out.status.success(),
        "ffprobe failed on {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    let json: Value = serde_json::from_slice(&out.stdout).expect("ffprobe JSON");
    parse_probe(&json)
}

fn num<T: std::str::FromStr>(v: &Value) -> Option<T> {
    match v {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.to_string().parse().ok(),
        _ => None,
    }
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

fn rate(v: &Value) -> Option<f64> {
    let s = v.as_str()?;
    let (n, d) = s.split_once('/')?;
    let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
    (d > 0.0 && n > 0.0).then(|| n / d)
}

pub fn parse_probe(json: &Value) -> ProbeInfo {
    let format = &json["format"];
    let streams = json["streams"]
        .as_array()
        .map(|a| a.iter().map(parse_stream).collect())
        .unwrap_or_default();
    ProbeInfo {
        container: format["format_name"]
            .as_str()
            .unwrap_or_default()
            .split(',')
            .next()
            .unwrap_or_default()
            .to_string(),
        format_long_name: text(&format["format_long_name"]),
        duration_secs: num(&format["duration"]),
        bit_rate: num(&format["bit_rate"]),
        size_bytes: num(&format["size"]).unwrap_or(0),
        start_time: num(&format["start_time"]),
        chapters: json["chapters"]
            .as_array()
            .map_or(0, |c| u32::try_from(c.len()).unwrap_or(0)),
        streams,
    }
}

fn parse_stream(s: &Value) -> StreamInfo {
    let kind = match s["codec_type"].as_str() {
        Some("video") => Some(StreamKind::Video),
        Some("audio") => Some(StreamKind::Audio),
        Some("subtitle") => Some(StreamKind::Subtitle),
        Some("attachment") => Some(StreamKind::Attachment),
        Some("data") => Some(StreamKind::Data),
        _ => None,
    };
    let disposition = &s["disposition"];
    let flag = |name: &str| disposition[name].as_i64() == Some(1);
    let transfer = text(&s["color_transfer"]);
    let hdr = match transfer.as_deref() {
        Some("smpte2084") => Some(HdrFormat::Hdr10),
        Some("arib-std-b67") => Some(HdrFormat::Hlg),
        _ => None,
    };
    let field_order = s["field_order"].as_str().unwrap_or("progressive");
    StreamInfo {
        index: num(&s["index"]).unwrap_or(0),
        kind,
        codec: text(&s["codec_name"]).unwrap_or_default(),
        profile: text(&s["profile"]),
        language: text(&s["tags"]["language"]),
        title: text(&s["tags"]["title"]),
        is_default: flag("default"),
        is_forced: flag("forced"),
        is_attached_pic: flag("attached_pic"),
        bit_rate: num(&s["bit_rate"]),
        width: num(&s["width"]),
        height: num(&s["height"]),
        pix_fmt: text(&s["pix_fmt"]),
        bit_depth: num(&s["bits_per_raw_sample"]),
        frame_rate: rate(&s["avg_frame_rate"]).or_else(|| rate(&s["r_frame_rate"])),
        color_primaries: text(&s["color_primaries"]),
        color_transfer: transfer,
        color_space: text(&s["color_space"]),
        color_range: text(&s["color_range"]),
        hdr,
        interlaced: matches!(field_order, "tt" | "bb" | "tb" | "bt"),
        channels: num(&s["channels"]),
        channel_layout: text(&s["channel_layout"]),
        sample_rate: num(&s["sample_rate"]),
    }
}

/// Run ffmpeg with `args`; returns stderr on failure.
pub fn run_ffmpeg(args: &[String]) -> Result<(), String> {
    let out = Command::new("ffmpeg")
        .args(args)
        .output()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    if out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if std::env::var_os("PLAN_SHOW_STDERR").is_some() && !stderr.trim().is_empty() {
            eprintln!("---- ffmpeg stderr for {:?}\n{stderr}", args.last());
        }
        Ok(())
    } else {
        Err(format!(
            "ffmpeg exited with {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}
