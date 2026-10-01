//! Helpers shared by the `plan_*` integration tests: stream builders, argument
//! inspection, probing with the scanner's parser, and synthetic media.

#![allow(
    dead_code,
    reason = "shared by several test binaries, each using part of it"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use chrysopoeia_core::{
    AudioCodec, Container, EncoderCandidate, Goal, HdrFormat, ProbeInfo, QualityLevel, SpeedPreset,
    StreamInfo, StreamKind, SubtitlePolicy, TranscodeProfile, VideoCodec,
};

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

/// The software downscale filter for a size limit of `long`x`short`
/// (either orientation), written out independently of the planner.
pub fn fit_filter(long: u32, short: u32) -> String {
    let (l, s) = (long, short);
    format!(
        "scale=w='if(gte(iw,ih),if(gte(iw*{s},ih*{l}),{l},-2),if(gte(ih*{s},iw*{l}),-2,{s}))'\
         :h='if(gte(iw,ih),if(gte(iw*{s},ih*{l}),-2,{s}),if(gte(ih*{s},iw*{l}),{l},-2))'\
         :flags=lanczos"
    )
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
    review_samples(dir, &ffmpeg)
}

/// Channel layouts of the tracks in `Layouts.mov`, in order, with their
/// channel counts. ffmpeg names `5.0(side)` what AC-3/DTS decoders report
/// for DVD 3/2 audio.
pub const LAYOUT_SAMPLE: &[(&str, u32)] = &[
    ("5.0(side)", 5),
    ("4.0", 4),
    ("quad", 4),
    ("6.1", 7),
    ("7.1(wide)", 8),
    ("2.1", 3),
];

/// Samples for the cases QA reviews found. Returns the name of the one that
/// failed.
fn review_samples(dir: &Path, ffmpeg: &dyn Fn(&[&str]) -> bool) -> Result<(), &'static str> {
    // A transport stream whose only audio starts 8 s in: a plain ffprobe
    // (like the scanner's) reports it as 0 channels at 0 Hz.
    let late = ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x240:rate=10:duration=10",
        "-itsoffset",
        "8",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=2:sample_rate=48000",
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-c:v",
        "mpeg2video",
        "-b:v",
        "500k",
        "-c:a",
        "ac3",
        "-f",
        "mpegts",
        "Late Audio.ts",
    ]);
    if !late {
        return Err("late audio");
    }

    // One PCM track per awkward channel layout (MOV keeps PCM layouts).
    let mut args: Vec<String> = [
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=160x120:rate=10:duration=3",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    for (n, (layout, channels)) in LAYOUT_SAMPLE.iter().enumerate() {
        let tones: Vec<String> = (0..*channels)
            .map(|c| format!("sin({}*2*PI*t)", 220 + 55 * (c + n as u32)))
            .collect();
        args.extend([
            "-f".to_string(),
            "lavfi".to_string(),
            "-i".to_string(),
            format!("aevalsrc={}:c={layout}:s=44100:d=3", tones.join("|")),
        ]);
    }
    args.extend(["-map".to_string(), "0:v".to_string()]);
    for n in 1..=LAYOUT_SAMPLE.len() {
        args.extend(["-map".to_string(), format!("{n}:a")]);
    }
    args.extend(
        [
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "pcm_s16le",
            "Layouts.mov",
        ]
        .iter()
        .map(|s| (*s).to_string()),
    );
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    if !ffmpeg(&refs) {
        return Err("channel layouts");
    }

    // Camera-style MOV: H.264, PCM sound and a timecode track.
    let camera = ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x240:rate=25:duration=3",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=300:duration=3:sample_rate=48000",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "pcm_s16le",
        "-timecode",
        "01:00:00:00",
        "Camera.mov",
    ]);
    if !camera {
        return Err("camera");
    }

    // A phone clip stored 1280x720 and shown rotated to portrait.
    let phone = ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=1280x720:rate=10:duration=3",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=350:duration=3",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        ".phone.mp4",
    ]) && ffmpeg(&[
        "-display_rotation",
        "90",
        "-i",
        ".phone.mp4",
        "-c",
        "copy",
        "Phone.mp4",
    ]);
    if !phone {
        return Err("rotated phone");
    }

    // Matroska written to a pipe: no duration anywhere.
    let piped = std::fs::File::create(dir.join("Piped.mkv")).is_ok_and(|file| {
        Command::new("ffmpeg")
            .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x240:rate=25:duration=3",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=450:duration=3",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-f",
                "matroska",
                "-",
            ])
            .stdout(file)
            .status()
            .is_ok_and(|s| s.success())
    });
    if !piped {
        return Err("piped Matroska");
    }

    // English audio description (original + visual impaired) and a German
    // default track.
    let dispositions = ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=160x120:rate=10:duration=3",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=300:duration=3",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=500:duration=3",
        "-map",
        "0",
        "-map",
        "1",
        "-map",
        "2",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-metadata:s:a:0",
        "language=eng",
        "-metadata:s:a:1",
        "language=ger",
        "-disposition:a:0",
        "original+visual_impaired",
        "-disposition:a:1",
        "default",
        "Dispositions.mkv",
    ]);
    if !dispositions {
        return Err("dispositions");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// ffprobe → ProbeInfo

/// Probe a file the way the server does: ffprobe's JSON read by the
/// scanner's own parser, so the planner is tested on the probe it gets.
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
    let size = std::fs::metadata(path).map_or(0, |m| m.len());
    chrysopoeia_scanner::parse_ffprobe_json(&out.stdout, size).expect("ffprobe JSON")
}

/// Run ffmpeg with `args`. Returns its warnings (stderr) on success and the
/// exit status plus stderr on failure.
pub fn run_ffmpeg(args: &[String]) -> Result<String, String> {
    let out = Command::new("ffmpeg")
        .args(args)
        .output()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;
    if out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        if std::env::var_os("PLAN_SHOW_STDERR").is_some() && !stderr.trim().is_empty() {
            eprintln!("---- ffmpeg stderr for {:?}\n{stderr}", args.last());
        }
        Ok(stderr)
    } else {
        Err(format!(
            "ffmpeg exited with {}\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ))
    }
}
