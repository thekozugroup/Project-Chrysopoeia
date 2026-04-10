//! Recursive directory scanner for media files.

use std::path::Path;

use chrono::Utc;
use chrysopeia_core::models::{MediaFile, MediaFileStatus, MediaFormat};
use uuid::Uuid;
use walkdir::WalkDir;

/// File extensions recognized as media files.
const MEDIA_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "mov", "webm", "ogg", "flac", "mp3", "m4a", "wav", "ts", "wmv", "flv",
];

/// Recursively scan a directory for media files.
///
/// Returns a list of `MediaFile` entries with basic metadata.
/// Each file is probed for codec information via `probe::probe_file`.
pub async fn scan_directory(dir: &Path) -> anyhow::Result<Vec<MediaFile>> {
    tracing::info!("Scanning directory: {}", dir.display());
    let mut files = Vec::new();

    for entry in WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        if !is_media_file(path) {
            continue;
        }

        let metadata = std::fs::metadata(path)?;
        let format = match crate::probe::probe_file(path).await {
            Ok(fmt) => fmt,
            Err(e) => {
                tracing::warn!("Failed to probe {}: {e}", path.display());
                continue;
            }
        };

        let status = if chrysopeia_core::codec::is_open_format(&format) {
            MediaFileStatus::OpenFormat
        } else {
            MediaFileStatus::NeedsTranscode
        };

        let now = Utc::now();
        files.push(MediaFile {
            id: Uuid::new_v4(),
            path: path.to_string_lossy().into_owned(),
            size: metadata.len(),
            format,
            status,
            created_at: now,
            updated_at: now,
        });
    }

    tracing::info!("Found {} media files in {}", files.len(), dir.display());
    Ok(files)
}

/// Check whether a file has a recognized media extension.
fn is_media_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| MEDIA_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
}
