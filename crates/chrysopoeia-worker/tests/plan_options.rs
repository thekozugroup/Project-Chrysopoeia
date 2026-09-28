//! Every encoder option, named option value and filter option the planner
//! emits must exist in this ffmpeg build.
//!
//! Hardware encoders cannot run here (no GPU), but ffmpeg still lists their
//! options with `-h encoder=<name>`, so option-name or value typos in the
//! NVENC/QSV/VA-API/V4L2 paths are caught without the hardware. Encoders
//! missing from the local build (AMF, VideoToolbox, Rockchip) are skipped.

mod common;

use std::collections::HashMap;
use std::process::Command;

use chrysopoeia_core::{
    AudioCodec, Container, QualityLevel, SpeedPreset, VIDEO_ENCODERS, VideoCodec,
};
use common::*;

/// Options every encoder accepts through `AVCodecContext` (they are not
/// listed per encoder).
const GENERIC: &[&str] = &["b", "profile", "global_quality", "q", "pix_fmt", "tag"];

/// Options that belong to ffmpeg itself or to the muxer/filters, not to the
/// video encoder.
const NOT_ENCODER: &[&str] = &[
    "c",
    "vf",
    "map",
    "map_metadata",
    "map_chapters",
    "metadata",
    "x265-params",
    "color_primaries",
    "color_trc",
    "colorspace",
    "disposition",
    "max_muxing_queue_size",
    "movflags",
];

/// Options every filter accepts (not listed per filter).
const GENERIC_FILTER: &[&str] = &["enable", "threads", "extra_hw_frames"];

/// `-h encoder=` / `-h filter=` output as option → named values.
fn help_options(kind: &str, name: &str) -> HashMap<String, Vec<String>> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-h", &format!("{kind}={name}")])
        .output()
        .expect("ffmpeg runs");
    let text = String::from_utf8_lossy(&out.stdout);
    let mut options: HashMap<String, Vec<String>> = HashMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let first = trimmed.split_whitespace().next().unwrap_or_default();
        if let Some(opt) = first.strip_prefix('-') {
            // "  -preset  <int>  ..." (encoders) — filters list "  w  <string>".
            current = Some(opt.to_string());
            options.entry(opt.to_string()).or_default();
        } else if kind == "filter" && indent <= 3 && trimmed.contains('<') {
            current = Some(first.to_string());
            options.entry(first.to_string()).or_default();
        } else if indent >= 5 && !first.is_empty() {
            if let Some(opt) = &current {
                options
                    .entry(opt.clone())
                    .or_default()
                    .push(first.to_string());
            }
        }
    }
    options
}

/// The value must be one of the option's named constants, unless it is a
/// number (or the option has no constants).
fn check_value(encoder: &str, opt: &str, value: &str, named: &[String]) -> Result<(), String> {
    let numeric = value.trim_end_matches('k').parse::<f64>().is_ok();
    if numeric || named.is_empty() || named.iter().any(|n| n == value) {
        Ok(())
    } else {
        Err(format!("{encoder}: -{opt} {value} is not one of {named:?}"))
    }
}

#[test]
fn encoder_options_exist_in_ffmpeg() {
    if !media_tools_available() {
        return;
    }
    let sources = [
        probe_of("matroska", vec![video(0, "h264", 1920, 1080)]),
        probe_of("matroska", vec![video_10bit(0, "hevc", 3840, 2160)]),
    ];
    let mut checked = 0;
    let mut problems = Vec::new();
    for enc in VIDEO_ENCODERS {
        if !has_encoder(enc.name) {
            eprintln!("not in this ffmpeg build, skipped: {}", enc.name);
            continue;
        }
        let help = help_options("encoder", enc.name);
        let container = if enc.codec == VideoCodec::Vp9 {
            Container::Webm
        } else {
            Container::Mkv
        };
        for source in &sources {
            for speed in [
                SpeedPreset::Fast,
                SpeedPreset::Balanced,
                SpeedPreset::Thorough,
            ] {
                let mut prof = profile(enc.codec, AudioCodec::Copy, container);
                prof.speed = speed;
                prof.quality = QualityLevel::High;
                let plan = plan(source, &prof, &candidate(enc.name, false));
                let args = &plan.args;
                let start = pos(args, "-c:v").map_or(0, |i| i + 2);
                let mut i = start;
                while i + 1 < args.len() && args[i].starts_with('-') {
                    let flag = &args[i][1..];
                    let value = &args[i + 1];
                    let opt = flag.split(':').next().unwrap_or(flag);
                    if NOT_ENCODER.contains(&opt) || opt == "f" {
                        i += 2;
                        continue;
                    }
                    match help.get(opt) {
                        Some(named) => {
                            if let Err(e) = check_value(enc.name, opt, value, named) {
                                problems.push(e);
                            }
                        }
                        None if GENERIC.contains(&opt) => {}
                        None => problems.push(format!("{}: unknown option -{opt}", enc.name)),
                    }
                    checked += 1;
                    i += 2;
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(checked > 0, "no encoder options were checked");
}

#[test]
fn filter_options_exist_in_ffmpeg() {
    if !media_tools_available() {
        return;
    }
    let mut v = video(0, "h264", 3840, 2160);
    v.interlaced = true;
    let interlaced = probe_of("mpegts", vec![v]);
    let tall = probe_of("matroska", vec![video(0, "h264", 3840, 2160)]);
    let odd = probe_of("matroska", vec![video(0, "vp9", 639, 359)]);
    let mut prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    prof.max_height = Some(1080);

    let mut chains = Vec::new();
    for (name, hw) in [
        ("libx265", false),
        ("hevc_nvenc", true),
        ("hevc_vaapi", true),
        ("hevc_vaapi", false),
        ("hevc_qsv", true),
        ("hevc_qsv", false),
    ] {
        for source in [&interlaced, &tall, &odd] {
            if let Some(chain) = vf(&plan(source, &prof, &candidate(name, hw)).args) {
                chains.push(chain);
            }
        }
    }
    let opus = plan(
        &probe_of(
            "matroska",
            vec![
                video(0, "h264", 1280, 720),
                audio(1, "ac3", 6, 48_000, None),
            ],
        ),
        &profile(VideoCodec::Av1, AudioCodec::Opus, Container::Mkv),
        &software(VideoCodec::Av1),
    );
    chains.push(value(&opus.args, "-filter:a:0").expect("opus layout filter"));

    let mut problems = Vec::new();
    for chain in &chains {
        for filter in chain.split(',') {
            let (name, params) = filter.split_once('=').unwrap_or((filter, ""));
            let help = help_options("filter", name);
            if help.is_empty() {
                problems.push(format!("filter {name} is not in this ffmpeg"));
                continue;
            }
            for param in params.split(':').filter(|p| p.contains('=')) {
                let (key, value) = param.split_once('=').unwrap_or((param, ""));
                match help.get(key) {
                    None if GENERIC_FILTER.contains(&key) => {}
                    None => problems.push(format!("{name}: unknown option {key}")),
                    Some(named) if key == "mode" || key == "flags" => {
                        if let Err(e) = check_value(name, key, value, named) {
                            problems.push(e);
                        }
                    }
                    Some(_) => {}
                }
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
