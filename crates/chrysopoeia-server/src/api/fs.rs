//! `/api/fs/browse`: the server-side folder picker.
//!
//! Only folders inside the configured browse roots are listed. The requested
//! path is checked lexically first (so `..` tricks are refused without
//! touching the disk), then canonicalized (so symlinks pointing outside a
//! root are refused too).

use std::path::{Component, Path, PathBuf};

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use super::extract::ApiQuery;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;
use crate::toolkit::Toolkit;

/// Subfolders whose media files are counted; beyond this the count is left
/// out so huge folders stay fast.
pub const MAX_COUNTED_DIRS: usize = 300;
/// Entries inspected per subfolder when counting media files.
pub const MAX_COUNTED_ENTRIES: usize = 2_000;

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
    /// Media files directly inside (not recursive), when counted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_count: Option<u64>,
}

/// Response of `GET /api/fs/browse`.
#[derive(Debug, Serialize)]
pub struct BrowseResponse {
    pub path: String,
    pub parent: Option<String>,
    pub roots: Vec<String>,
    pub entries: Vec<BrowseEntry>,
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

async fn canonical_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(roots.len());
    for r in roots {
        if let Ok(c) = tokio::fs::canonicalize(r).await
            && !out.contains(&c)
        {
            out.push(c);
        }
    }
    out
}

/// Count media files directly inside `dir`. `Err` when the folder can't be
/// read; `Ok(None)` when counting isn't possible.
fn count_media(dir: &Path, toolkit: &Toolkit, count: bool) -> std::io::Result<Option<u64>> {
    let entries = std::fs::read_dir(dir)?;
    if !count {
        return Ok(None);
    }
    let mut n = 0u64;
    for entry in entries.take(MAX_COUNTED_ENTRIES).flatten() {
        let is_file = entry
            .file_type()
            .map(|t| t.is_file() || t.is_symlink())
            .unwrap_or(false);
        if !is_file {
            continue;
        }
        match toolkit.is_media_path_blocking(&entry.path()) {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(_) => return Ok(None),
        }
    }
    Ok(Some(n))
}

/// List the visible, readable subfolders of `dir`.
fn list_dirs(
    dir: &Path,
    roots: &[PathBuf],
    toolkit: &Toolkit,
) -> std::io::Result<Vec<BrowseEntry>> {
    let mut out = Vec::new();
    let mut counting = true;
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
            continue;
        }
        let count = counting && out.len() < MAX_COUNTED_DIRS;
        let media_count = match count_media(&path, toolkit, count) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if count && media_count.is_none() {
            counting = false;
        }
        let Some(path_str) = path.to_str() else {
            continue;
        };
        out.push(BrowseEntry {
            name,
            path: path_str.to_string(),
            is_dir: true,
            media_count,
        });
    }
    out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

/// `GET /api/fs/browse`
pub async fn browse(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<BrowseQuery>,
) -> ApiResult<Json<BrowseResponse>> {
    let roots = canonical_roots(&state.config.browse_roots).await;
    let Some(first_root) = roots.first().cloned() else {
        return Err(ApiError::not_found(
            "no_browse_roots",
            "None of the folders the picker may show exist on the server. Check BROWSE_ROOTS.",
        ));
    };
    let requested = match q.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => first_root,
    };
    if !requested.is_absolute() {
        return Err(ApiError::bad_request(
            "path_not_absolute",
            "Use a full folder path, starting with /.",
        ));
    }
    let lexical = lexical_normalize(&requested);
    let lexical_ok = within(&lexical, &roots) || within(&lexical, &state.config.browse_roots);
    if !lexical_ok {
        return Err(outside_roots());
    }
    let canonical = match tokio::fs::canonicalize(&lexical).await {
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
    if !tokio::fs::metadata(&canonical)
        .await
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
    let toolkit = state.toolkit.clone();
    let dir = canonical.clone();
    let list_roots = roots.clone();
    let entries = tokio::task::spawn_blocking(move || list_dirs(&dir, &list_roots, &toolkit))
        .await
        .map_err(ApiError::internal)?
        .map_err(|_| {
            ApiError::bad_request(
                "not_readable",
                "Chrysopoeia can't read that folder. Check its permissions.",
            )
        })?;
    Ok(Json(BrowseResponse {
        path: canonical.to_string_lossy().into_owned(),
        parent,
        roots: roots
            .iter()
            .map(|r| r.to_string_lossy().into_owned())
            .collect(),
        entries,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

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
