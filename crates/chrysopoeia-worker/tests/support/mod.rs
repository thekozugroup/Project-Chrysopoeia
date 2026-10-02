//! Shared helpers for the worker integration tests: synthetic media,
//! probing with the scanner's own parser, and ffmpeg wrappers. Tests that
//! need ffmpeg call [`require_ffmpeg!`] first and are skipped when it is not
//! installed.

#![allow(
    dead_code,
    reason = "shared by several test binaries, each using part of it"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use chrysopoeia_core::{ProbeInfo, StreamKind};
use chrysopoeia_worker::StreamSummary;

/// Skip the current test (with a note) when ffmpeg/ffprobe are missing.
#[macro_export]
macro_rules! require_ffmpeg {
    () => {
        if !support::ffmpeg_available() {
            eprintln!("ffmpeg/ffprobe not found on PATH; skipping this test");
            return;
        }
    };
}

/// Clip length used for generated media, in seconds.
pub const CLIP_SECS: u32 = 4;

pub const MP4_TWO_AUDIO: &str = "Movies/Big Test (2020)/Big Test (2020).mp4";
pub const MKV_1080P_SUBS: &str = "TV/Show/Season 01/Show - S01E01.mkv";
pub const MKV_HEVC_10BIT: &str = "TV/Show/Season 01/Show - S01E02.mkv";
pub const TS_INTERLACED: &str = "TV/Show/Season 01/Show - S01E03.ts";
pub const AVI_ODD_SIZE: &str = "Movies/Old Home Video.avi";

/// Whether ffmpeg and ffprobe can be run.
pub fn ffmpeg_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        ["ffmpeg", "ffprobe"].iter().all(|tool| {
            Command::new(tool)
                .arg("-version")
                .output()
                .is_ok_and(|o| o.status.success())
        })
    })
}

/// The synthetic library from `scripts/make-test-media.sh`, generated once
/// per test binary under Cargo's integration-test scratch directory.
pub fn test_media() -> &'static Path {
    static MEDIA: OnceLock<PathBuf> = OnceLock::new();
    MEDIA.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/make-test-media.sh");
        // The cache is keyed by the script's contents, so a changed script
        // never serves stale media.
        let version = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            std::fs::read(&script).unwrap_or_default().hash(&mut h);
            h.finish()
        };
        let dir = root.join(format!("worker-media-{CLIP_SECS}s-{version:016x}"));
        if dir.join(".complete").exists() {
            return dir;
        }
        let staging = tempfile::Builder::new()
            .prefix("worker-media-")
            .tempdir_in(&root)
            .expect("create staging dir");
        let status = Command::new("sh")
            .arg(&script)
            .arg(staging.path())
            .arg(CLIP_SECS.to_string())
            .status()
            .expect("run make-test-media.sh");
        assert!(status.success(), "make-test-media.sh failed");
        std::fs::write(staging.path().join(".complete"), b"").expect("marker");
        let staged = staging.keep();
        // Another test binary may have finished first; either copy is fine.
        if std::fs::rename(&staged, &dir).is_err() {
            let _ = std::fs::remove_dir_all(&staged);
        }
        dir
    })
}

/// Copy one test clip into `dir` (tests modify their copies freely).
pub fn copy_media(name: &str, dir: &Path) -> PathBuf {
    let src = test_media().join(name);
    let file_name = src.file_name().expect("file name");
    let dst = dir.join(file_name);
    std::fs::copy(&src, &dst).unwrap_or_else(|e| panic!("copy {}: {e}", src.display()));
    dst
}

/// Run ffmpeg quietly with `-y`; panics on failure.
pub fn ffmpeg(args: &[&str]) {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .args(args)
        .output()
        .expect("run ffmpeg");
    assert!(
        output.status.success(),
        "ffmpeg {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Probe a file the way the server does: ffprobe's JSON read by the
/// scanner's own parser (HDR metadata, bit depth and all).
pub fn probe(path: &Path) -> ProbeInfo {
    let output = Command::new("ffprobe")
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
        .expect("run ffprobe");
    assert!(
        output.status.success(),
        "ffprobe failed on {}",
        path.display()
    );
    let size = std::fs::metadata(path).map_or(0, |m| m.len());
    chrysopoeia_scanner::parse_ffprobe_json(&output.stdout, size).expect("ffprobe json")
}

/// Stream counts of a probe (video excludes cover art).
pub fn counts(probe: &ProbeInfo) -> StreamSummary {
    let count = |kind| {
        probe
            .streams
            .iter()
            .filter(|s| s.kind == Some(kind) && !s.is_attached_pic)
            .count() as u32
    };
    StreamSummary {
        video: count(StreamKind::Video),
        audio: count(StreamKind::Audio),
        subtitle: count(StreamKind::Subtitle),
    }
}

/// Files in `dir` whose names mark them as Chrysopoeia temp/backup files.
pub fn artifacts_in(dir: &Path) -> Vec<PathBuf> {
    walk(dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(chrysopoeia_core::paths::is_artifact)
        })
        .collect()
}

/// Every file under `dir`, recursively.
pub fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Overwrite `len` bytes in the middle of a file with a fixed pseudo-random
/// pattern (reproducible corruption).
pub fn corrupt_middle(path: &Path, len: usize) {
    let mut data = std::fs::read(path).expect("read file to corrupt");
    let start = data.len() / 2;
    let end = (start + len).min(data.len());
    let mut x: u32 = 0x1234_5678;
    for byte in &mut data[start..end] {
        // xorshift32
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        *byte = (x & 0xff) as u8;
    }
    std::fs::write(path, data).expect("write corrupted file");
}
