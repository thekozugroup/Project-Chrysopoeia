//! Library scanning: walking folders, probing files with ffprobe, and
//! watching folders for changes.
//!
//! Public API (fixed; see docs/ARCHITECTURE.md):
//! - [`walk_library`] — list media files under a root (blocking; call from
//!   `spawn_blocking`).
//! - [`probe_file`] / [`parse_ffprobe_json`] — stream-level probe.
//! - [`LibraryWatcher`] — debounced create/modify/remove events.
//!
//! Additions beyond the fixed API: [`IgnoreRules`] and
//! [`validate_ignore_pattern`] (the ignore-pattern matcher the walker uses,
//! for filtering watcher events and validating settings),
//! [`ScanOptions::from_settings`], [`LibraryWatcher::watch_with_options`],
//! and the disc-copy folder names [`DVD_FOLDERS`] and [`BLU_RAY_FOLDERS`].
//!
//! Safety rules shared by the walker and the watcher: DVD and Blu-ray disc
//! copies are never listed (their files only work together), symbolic links
//! are not followed unless asked, and Chrysopoeia's own temporary and backup
//! files are reported separately, never as media.

#![warn(missing_docs)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use chrysopoeia_core::{ProbeInfo, Settings};

pub mod probe;
pub mod walk;
pub mod watch;

pub use walk::{
    AUDIO_EXTENSIONS, BLU_RAY_FOLDERS, DVD_FOLDERS, IgnoreRules, VIDEO_EXTENSIONS,
    validate_ignore_pattern,
};
pub use watch::{LibraryWatcher, WaitingFiles, WatchEvent};

/// Bytes in the megabyte of `Settings::min_file_size_mb` (decimal, like
/// the "MB" the UI shows).
pub const BYTES_PER_MB: u64 = 1_000_000;

/// Options for [`walk_library`].
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Glob patterns matched against the path relative to the library root.
    /// See the [`walk`] module for the exact rules.
    pub ignore_patterns: Vec<String>,
    /// Skip files smaller than this.
    pub min_size_bytes: u64,
    /// Follow symbolic links to files and folders. Link loops are detected
    /// and reported instead of being followed forever. When false, links are
    /// skipped entirely.
    pub follow_links: bool,
}

impl ScanOptions {
    /// Options matching the user's settings: their ignore patterns and
    /// minimum file size. `min_file_size_mb` is in decimal megabytes
    /// ([`BYTES_PER_MB`]), as the UI shows it. Links are not followed.
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            ignore_patterns: settings.ignore_patterns.clone(),
            min_size_bytes: u64::from(settings.min_file_size_mb) * BYTES_PER_MB,
            follow_links: false,
        }
    }
}

/// A media file found on disk (not yet probed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFile {
    /// Full path: the library root joined with the path below it.
    pub path: PathBuf,
    /// Size in bytes.
    pub size: u64,
    /// Last modification time (the Unix epoch if the platform cannot tell).
    pub modified: DateTime<Utc>,
}

/// Result of walking a library root.
#[derive(Debug, Clone, Default)]
pub struct WalkResult {
    /// Media files, sorted by path.
    pub files: Vec<DiscoveredFile>,
    /// Chrysopoeia temp/backup files found (see `chrysopoeia_core::paths`).
    pub artifacts: Vec<PathBuf>,
    /// Paths that could not be read (permissions, read errors, entries that
    /// vanished mid-walk), with a plain-language reason. Nothing below these
    /// paths is listed, so their absence from `files` does not mean they
    /// were deleted.
    pub errors: Vec<(PathBuf, String)>,
    /// Paths deliberately left alone, with a plain-language note: DVD and
    /// Blu-ray disc copies, links that are not followed, names that aren't
    /// valid UTF-8, and ignore patterns that can't be used (noted on the
    /// root). Informational, not failures; nothing below them is listed
    /// either.
    pub notes: Vec<(PathBuf, String)>,
}

/// Why a file could not be probed.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// ffprobe could not be started (missing or not executable).
    #[error("could not run ffprobe: {0}")]
    Spawn(String),
    /// ffprobe did not finish in time and was killed.
    #[error("ffprobe timed out after {0:?}")]
    Timeout(Duration),
    /// ffprobe ran but rejected the file (corrupt or not media).
    #[error("{0}")]
    Unreadable(String),
    /// ffprobe's output could not be understood.
    #[error("could not parse ffprobe output: {0}")]
    Parse(String),
}

/// Whether a path has a media file extension we handle.
///
/// Covers essentially every video container ffmpeg reads plus common audio
/// containers (see [`VIDEO_EXTENSIONS`] and [`AUDIO_EXTENSIONS`]), ignoring
/// case. Subtitles, images, text and Chrysopoeia's own temporary or backup
/// files are never media, and neither are the files of a DVD or Blu-ray disc
/// copy (anything inside a [`DVD_FOLDERS`] or [`BLU_RAY_FOLDERS`] folder),
/// which only play correctly together.
pub fn is_media_path(path: &Path) -> bool {
    walk::is_media(path)
}

/// Whether a path is a video file Chrysopoeia would convert: like
/// [`is_media_path`], but only [`VIDEO_EXTENSIONS`] (music and other
/// audio-only files are left out). The folder picker counts these.
pub fn is_video_path(path: &Path) -> bool {
    walk::is_video(path)
}

/// Walk `root` recursively. Errors only if `root` itself is unreadable.
///
/// Blocking: call it from `spawn_blocking`. Nothing is probed, so large
/// libraries are listed quickly. Problems with individual entries are
/// collected in [`WalkResult::errors`], together with notes on what was
/// deliberately left alone (disc copies, links); see the [`walk`] module.
pub fn walk_library(root: &Path, opts: &ScanOptions) -> anyhow::Result<WalkResult> {
    walk::walk(root, opts)
}

/// Probe one file with ffprobe. Kills ffprobe after `timeout`.
pub async fn probe_file(
    ffprobe: &Path,
    path: &Path,
    timeout: Duration,
) -> Result<ProbeInfo, ProbeError> {
    probe::probe(ffprobe, path, timeout).await
}

/// Parse `ffprobe -print_format json -show_format -show_streams -show_chapters`
/// output. `size_bytes` is the file size from the filesystem.
pub fn parse_ffprobe_json(json: &[u8], size_bytes: u64) -> Result<ProbeInfo, ProbeError> {
    probe::parse(json, size_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_from_settings() {
        let settings = Settings {
            min_file_size_mb: 50,
            ..Settings::default()
        };
        let opts = ScanOptions::from_settings(&settings);
        assert_eq!(opts.min_size_bytes, 50_000_000);
        assert_eq!(opts.ignore_patterns, settings.ignore_patterns);
        assert!(!opts.follow_links);
    }
}
