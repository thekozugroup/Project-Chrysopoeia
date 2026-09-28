//! Output paths and crash-safe replacement of originals.
//! Implemented by the worker-run agent.

use std::path::{Path, PathBuf};

use chrysopoeia_core::{Container, OutputMode};
use uuid::Uuid;

/// Where the finished file will live.
///
/// - `Replace`: next to the input, with the container's extension.
/// - `Folder`: `output_folder` + the input's path relative to `library_root`,
///   with the container's extension.
pub fn final_output_path(
    input: &Path,
    container: Container,
    mode: OutputMode,
    output_folder: Option<&Path>,
    library_root: &Path,
) -> PathBuf {
    let _ = (input, container, mode, output_folder, library_root);
    todo!("implemented by the worker-run agent")
}

/// Hidden temp path for an in-progress encode, in `temp_dir` or next to the
/// input. Name from `chrysopoeia_core::paths::temp_file_name`.
pub fn temp_output_path(
    input: &Path,
    container: Container,
    job_id: Uuid,
    temp_dir: Option<&Path>,
) -> PathBuf {
    let _ = (input, container, job_id, temp_dir);
    todo!("implemented by the worker-run agent")
}

/// What [`recover_artifact`] did with a leftover file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recovery {
    /// A stale temp file was deleted.
    DeletedTemp,
    /// The original was missing, so the backup was restored in its place.
    RestoredBackup(PathBuf),
    /// The replacement was already in place, so the backup was deleted.
    DeletedBackup,
    Untouched,
}

/// Clean up a temp or backup file left behind by a crash. Safe to call at
/// startup for every artifact the scanner reports.
pub async fn recover_artifact(path: &Path) -> anyhow::Result<Recovery> {
    let _ = path;
    todo!("implemented by the worker-run agent")
}
