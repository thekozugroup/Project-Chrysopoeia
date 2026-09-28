//! Library scanning: walking folders, probing files with ffprobe, and
//! watching folders for changes.
//!
//! Public API (fixed; see docs/ARCHITECTURE.md):
//! - [`walk_library`] — list media files under a root (blocking; call from
//!   `spawn_blocking`).
//! - [`probe_file`] / [`parse_ffprobe_json`] — stream-level probe.
//! - [`LibraryWatcher`] — debounced create/modify/remove events.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use chrysopoeia_core::ProbeInfo;

pub mod probe;
pub mod walk;
pub mod watch;

pub use watch::{LibraryWatcher, WatchEvent};

/// Options for [`walk_library`].
#[derive(Debug, Clone, Default)]
pub struct ScanOptions {
    /// Glob patterns matched against the path relative to the library root.
    pub ignore_patterns: Vec<String>,
    /// Skip files smaller than this.
    pub min_size_bytes: u64,
    pub follow_links: bool,
}

/// A media file found on disk (not yet probed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFile {
    pub path: PathBuf,
    pub size: u64,
    pub modified: DateTime<Utc>,
}

/// Result of walking a library root.
#[derive(Debug, Clone, Default)]
pub struct WalkResult {
    pub files: Vec<DiscoveredFile>,
    /// Chrysopoeia temp/backup files found (see `chrysopoeia_core::paths`).
    pub artifacts: Vec<PathBuf>,
    /// Paths that could not be read, with the reason.
    pub errors: Vec<(PathBuf, String)>,
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("could not run ffprobe: {0}")]
    Spawn(String),
    #[error("ffprobe timed out after {0:?}")]
    Timeout(Duration),
    /// ffprobe ran but rejected the file (corrupt or not media).
    #[error("{0}")]
    Unreadable(String),
    #[error("could not parse ffprobe output: {0}")]
    Parse(String),
}

/// Whether a path has a media file extension we handle.
pub fn is_media_path(path: &Path) -> bool {
    let _ = path;
    todo!("implemented by the scanner agent")
}

/// Walk `root` recursively. Errors only if `root` itself is unreadable.
pub fn walk_library(root: &Path, opts: &ScanOptions) -> anyhow::Result<WalkResult> {
    let _ = (root, opts);
    todo!("implemented by the scanner agent")
}

/// Probe one file with ffprobe. Kills ffprobe after `timeout`.
pub async fn probe_file(
    ffprobe: &Path,
    path: &Path,
    timeout: Duration,
) -> Result<ProbeInfo, ProbeError> {
    let _ = (ffprobe, path, timeout);
    todo!("implemented by the scanner agent")
}

/// Parse `ffprobe -print_format json -show_format -show_streams -show_chapters`
/// output. `size_bytes` is the file size from the filesystem.
pub fn parse_ffprobe_json(json: &[u8], size_bytes: u64) -> Result<ProbeInfo, ProbeError> {
    let _ = (json, size_bytes);
    todo!("implemented by the scanner agent")
}
