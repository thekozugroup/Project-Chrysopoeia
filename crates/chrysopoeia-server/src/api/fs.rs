//! `/api/fs/browse`: the server-side folder picker.
//!
//! Only folders inside the configured browse roots are listed. The requested
//! path is checked lexically first (so `..` tricks are refused without
//! touching the disk), then canonicalized (so symlinks pointing outside a
//! root are refused too).

use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use super::extract::ApiQuery;
use crate::error::{ApiError, ApiResult};
use crate::services::fs_guard;
use crate::state::AppState;
use crate::toolkit::Toolkit;

/// Subfolders whose video files are counted; beyond this the count is left
/// out so huge folders stay fast.
pub const MAX_COUNTED_DIRS: usize = 300;
/// How many folder levels below a listed folder its video files are counted
/// (a show folder's seasons, a collection's movie folders).
pub const MAX_COUNT_DEPTH: usize = 4;
/// Entries (files and folders) looked at per listed folder when counting.
pub const MAX_COUNTED_ENTRIES: usize = 2_000;
/// Time spent counting per listed folder.
pub const COUNT_TIME_PER_FOLDER: Duration = Duration::from_millis(150);
/// Time spent counting for a whole listing; later folders get no count, so
/// a slow disk or share never keeps the folder picker waiting.
pub const COUNT_TIME_TOTAL: Duration = Duration::from_secs(2);

/// Query of `GET /api/fs/browse`.
#[derive(Debug, Default, Deserialize)]
pub struct BrowseQuery {
    pub path: Option<String>,
}

/// One subfolder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BrowseEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    /// Video files inside, down to [`MAX_COUNT_DEPTH`] folder levels (music
    /// and other audio-only files are not counted), when counted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_count: Option<u64>,
    /// Present with `media_count`: true when counting stopped at a limit
    /// (depth, entries or time), so the folder holds at least that many.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_count_capped: Option<bool>,
}

/// Response of `GET /api/fs/browse`.
#[derive(Debug, Clone, Serialize)]
pub struct BrowseResponse {
    pub path: String,
    pub parent: Option<String>,
    pub roots: Vec<String>,
    pub entries: Vec<BrowseEntry>,
    /// Video files in the browsed folder itself plus its subfolders' counts
    /// (links to folders, which a scan doesn't follow, left out), so the
    /// picker can say what choosing this folder brings. Never less than a
    /// subfolder's count; capped when any subfolder's is, or has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_count: Option<u64>,
    /// Present with `media_count`: counting stopped at a limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_count_capped: Option<bool>,
}

fn outside_roots() -> ApiError {
    ApiError::forbidden(
        "outside_roots",
        "That folder is outside the folders Chrysopoeia may show.",
    )
}

/// Resolve `.` and `..` without touching the filesystem.
pub fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn within(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|r| path.starts_with(r))
}

fn canonical_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(roots.len());
    for r in roots {
        if let Ok(c) = std::fs::canonicalize(r)
            && !out.contains(&c)
        {
            out.push(c);
        }
    }
    out
}

/// Video files found below a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VideoCount {
    videos: u64,
    /// Counting stopped at a limit.
    capped: bool,
}

/// Count the video files in `dir` and the folders below it, level by level
/// down to [`MAX_COUNT_DEPTH`] levels, stopping after `max_entries` entries
/// or once `budget` has passed. Hidden entries are skipped and links to
/// folders are not followed. `Err` when `dir` itself can't be read;
/// `Ok(None)` when `count` is false or counting isn't possible.
fn count_videos(
    dir: &Path,
    toolkit: &Toolkit,
    count: bool,
    max_entries: usize,
    budget: Duration,
) -> std::io::Result<Option<VideoCount>> {
    let top = std::fs::read_dir(dir)?;
    if !count {
        return Ok(None);
    }
    let started = Instant::now();
    let mut videos = 0u64;
    let mut seen = 0usize;
    let mut capped = false;
    let mut below: VecDeque<(PathBuf, usize)> = VecDeque::new();
    let mut current = Some((top, 0usize));
    'folders: loop {
        let (entries, depth) = match current.take() {
            Some(c) => c,
            None => match below.pop_front() {
                Some((path, depth)) => match std::fs::read_dir(&path) {
                    Ok(entries) => (entries, depth),
                    Err(_) => continue,
                },
                None => break,
            },
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > max_entries || started.elapsed() > budget {
                capped = true;
                break 'folders;
            }
            if entry
                .file_name()
                .to_str()
                .is_none_or(|n| n.starts_with('.'))
            {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if depth < MAX_COUNT_DEPTH {
                    below.push_back((entry.path(), depth + 1));
                } else {
                    capped = true;
                }
                continue;
            }
            if !(file_type.is_file() || file_type.is_symlink()) {
                continue;
            }
            match toolkit.is_video_path_blocking(&entry.path()) {
                Ok(true) => videos += 1,
                Ok(false) => {}
                Err(_) => return Ok(None),
            }
        }
    }
    Ok(Some(VideoCount { videos, capped }))
}

/// The visible, readable subfolders of `dir`, and the video files in `dir`
/// counted from them (see [`BrowseResponse::media_count`]).
struct Listing {
    entries: Vec<BrowseEntry>,
    own: Option<VideoCount>,
}

/// List the visible, readable subfolders of `dir`, counting the video files
/// in each and, from those, in `dir` itself.
fn list_dirs(dir: &Path, roots: &[PathBuf], toolkit: &Toolkit) -> std::io::Result<Listing> {
    let mut out = Vec::new();
    let mut counting = true;
    let started = Instant::now();
    // Summed from the direct files and the subfolders' counts, so the folder
    // never shows fewer videos than one of its subfolders.
    let mut own = Some(VideoCount {
        videos: 0,
        capped: false,
    });
    for entry in std::fs::read_dir(dir)?.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let is_dir = if file_type.is_symlink() {
            match std::fs::canonicalize(&path) {
                Ok(target) => within(&target, roots) && target.is_dir(),
                Err(_) => false,
            }
        } else {
            file_type.is_dir()
        };
        if !is_dir {
            if (file_type.is_file() || file_type.is_symlink())
                && let Some(c) = own.as_mut()
            {
                match toolkit.is_video_path_blocking(&path) {
                    Ok(true) => c.videos += 1,
                    Ok(false) => {}
                    Err(_) => own = None,
                }
            }
            continue;
        }
        let left = COUNT_TIME_TOTAL.saturating_sub(started.elapsed());
        let count = counting && out.len() < MAX_COUNTED_DIRS && !left.is_zero();
        let budget = COUNT_TIME_PER_FOLDER.min(left);
        let counted = match count_videos(&path, toolkit, count, MAX_COUNTED_ENTRIES, budget) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if count && counted.is_none() {
            counting = false;
        }
        let Some(path_str) = path.to_str() else {
            continue;
        };
        // A scan doesn't follow links, so a linked folder adds nothing.
        if !file_type.is_symlink()
            && let Some(c) = own.as_mut()
        {
            match counted {
                Some(sub) => {
                    c.videos += sub.videos;
                    c.capped |= sub.capped;
                }
                None => c.capped = true,
            }
        }
        out.push(BrowseEntry {
            name,
            path: path_str.to_string(),
            is_dir: true,
            media_count: counted.map(|c| c.videos),
            media_count_capped: counted.map(|c| c.capped),
        });
    }
    out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(Listing { entries: out, own })
}

/// How long a folder listing may take before the folder is reported as not
/// responding (a share whose server went away never answers).
const BROWSE_TIMEOUT: Duration = Duration::from_secs(10);

/// `GET /api/fs/browse`
pub async fn browse(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<BrowseQuery>,
) -> ApiResult<Json<BrowseResponse>> {
    let requested = q
        .path
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    if requested.as_ref().is_some_and(|p| !p.is_absolute()) {
        return Err(ApiError::bad_request(
            "path_not_absolute",
            "Use a full folder path, starting with /.",
        ));
    }
    // The listing runs on one thread per folder at a time (see `fs_guard`),
    // so a hung share can't use up the server's threads.
    let key = requested
        .as_deref()
        .map_or_else(PathBuf::new, lexical_normalize);
    let config_roots = state.config.browse_roots.clone();
    let toolkit = state.toolkit.clone();
    let listed = fs_guard::guarded("browse", &key, BROWSE_TIMEOUT, move || {
        browse_blocking(requested, &config_roots, &toolkit)
    })
    .await;
    match listed {
        Some(result) => result.map(Json),
        None => Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "not_responding",
            "That folder isn't responding. If it's on a network share or an external drive, \
             check the connection.",
        )),
    }
}

/// [`browse`]'s work, with blocking calls.
fn browse_blocking(
    requested: Option<PathBuf>,
    config_roots: &[PathBuf],
    toolkit: &Toolkit,
) -> ApiResult<BrowseResponse> {
    let roots = canonical_roots(config_roots);
    let Some(first_root) = roots.first().cloned() else {
        return Err(ApiError::not_found(
            "no_browse_roots",
            "None of the folders the picker may show exist on the server. Check BROWSE_ROOTS.",
        ));
    };
    let requested = requested.unwrap_or(first_root);
    let lexical = lexical_normalize(&requested);
    let lexical_ok = within(&lexical, &roots) || within(&lexical, config_roots);
    if !lexical_ok {
        return Err(outside_roots());
    }
    let canonical = match std::fs::canonicalize(&lexical) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ApiError::not_found(
                "path_not_found",
                "That folder doesn't exist.",
            ));
        }
        Err(_) => {
            return Err(ApiError::bad_request(
                "not_readable",
                "Chrysopoeia can't open that folder. Check its permissions.",
            ));
        }
    };
    if !within(&canonical, &roots) {
        return Err(outside_roots());
    }
    if !std::fs::metadata(&canonical)
        .map(|m| m.is_dir())
        .unwrap_or(false)
    {
        return Err(ApiError::bad_request(
            "not_a_directory",
            "That's a file, not a folder.",
        ));
    }
    let parent = canonical
        .parent()
        .filter(|p| within(p, &roots))
        .and_then(|p| p.to_str())
        .map(str::to_string);
    let Listing { entries, own } = list_dirs(&canonical, &roots, toolkit).map_err(|_| {
        ApiError::bad_request(
            "not_readable",
            "Chrysopoeia can't read that folder. Check its permissions (in Docker, the \
             PUID/PGID user needs read access).",
        )
    })?;
    Ok(BrowseResponse {
        path: canonical.to_string_lossy().into_owned(),
        parent,
        roots: roots
            .iter()
            .map(|r| r.to_string_lossy().into_owned())
            .collect(),
        entries,
        media_count: own.map(|c| c.videos),
        media_count_capped: own.map(|c| c.capped),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_counts_go_down_the_levels_and_stop_at_the_limits() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let show = dir.path().join("Show");
        let touch = |rel: &str| {
            let p = show.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        };
        touch("Season 1/e1.mkv");
        touch("Season 1/e2.mp4");
        touch("Season 2/e1.mkv");
        touch("Season 2/e1.srt");
        touch("Soundtrack/01.flac");
        touch("Soundtrack/02.mp3");
        touch(".hidden/x.mkv");
        let toolkit = Toolkit::new(Arc::new(crate::toolkit::RealToolkit::new(PathBuf::from(
            "ffprobe",
        ))));
        let long = Duration::from_secs(5);
        let count = count_videos(&show, &toolkit, true, MAX_COUNTED_ENTRIES, long).unwrap();
        assert_eq!(
            count,
            Some(VideoCount {
                videos: 3,
                capped: false
            })
        );
        // Too many entries: at least what was seen, marked as capped.
        let count = count_videos(&show, &toolkit, true, 3, long)
            .unwrap()
            .unwrap();
        assert!(count.capped);
        assert!(count.videos <= 3);
        // Deeper than the depth limit.
        touch("a/b/c/d/e/deep.mkv");
        let count = count_videos(&show, &toolkit, true, MAX_COUNTED_ENTRIES, long)
            .unwrap()
            .unwrap();
        assert_eq!(count.videos, 3);
        assert!(count.capped);
        // Not counted at all; a missing folder is an error.
        assert_eq!(
            count_videos(&show, &toolkit, false, MAX_COUNTED_ENTRIES, long).unwrap(),
            None
        );
        assert!(count_videos(&show.join("nope"), &toolkit, true, 10, long).is_err());
    }

    /// The folder's own count is summed from its subfolders' counts, so it
    /// is never lower than one of them, even when a subfolder is too big to
    /// count fully.
    #[test]
    fn the_folder_count_includes_its_subfolders() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let touch = |rel: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        };
        touch("top.mkv");
        touch("notes.txt");
        touch("lib/a.mkv");
        touch("lib/b.mp4");
        for i in 0..(MAX_COUNTED_ENTRIES + 100) {
            touch(&format!("big/sub/e{i}.mkv"));
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("lib"), root.join("linked")).unwrap();
        let toolkit = Toolkit::new(Arc::new(crate::toolkit::RealToolkit::new(PathBuf::from(
            "ffprobe",
        ))));
        let roots = [root.canonicalize().unwrap()];
        let listing = list_dirs(&roots[0], &roots, &toolkit).unwrap();
        let count_of = |name: &str| {
            listing
                .entries
                .iter()
                .find(|e| e.name == name)
                .and_then(|e| e.media_count)
                .unwrap()
        };
        let own = listing.own.unwrap();
        assert!(own.capped);
        assert_eq!(count_of("lib"), 2);
        let big = count_of("big");
        assert!(big > 0, "{big}");
        // top.mkv + lib + big; the linked folder isn't scanned, so not counted.
        assert_eq!(own.videos, 1 + 2 + big);
        for entry in &listing.entries {
            assert!(entry.media_count.unwrap_or(0) <= own.videos, "{entry:?}");
        }

        // A small folder: exact.
        let listing = list_dirs(&roots[0].join("lib"), &roots, &toolkit).unwrap();
        assert_eq!(
            listing.own,
            Some(VideoCount {
                videos: 2,
                capped: false
            })
        );
    }

    #[test]
    fn lexical_normalization() {
        assert_eq!(
            lexical_normalize(Path::new("/media/../etc")),
            PathBuf::from("/etc")
        );
        assert_eq!(
            lexical_normalize(Path::new("/media/./Movies/")),
            PathBuf::from("/media/Movies")
        );
        assert_eq!(
            lexical_normalize(Path::new("/../../..")),
            PathBuf::from("/")
        );
    }
}
