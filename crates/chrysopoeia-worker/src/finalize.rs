//! Output paths and crash-safe replacement of originals.
//!
//! The encoder writes to a hidden temp file ([`temp_output_path`]). Only after
//! verification does [`finalize`] move it to its final place
//! ([`final_output_path`]):
//!
//! - Replace mode, same path: the original is renamed to a hidden backup in
//!   its own folder, the new file is moved in, and the backup is deleted. Any
//!   failure puts the backup back.
//! - Replace mode, new extension: refuses to overwrite an unrelated file,
//!   moves the new file in, then deletes the original.
//! - Folder mode: creates the folders, refuses to overwrite anything and never
//!   touches the original.
//!
//! Moves across filesystems copy into a hidden file next to the destination,
//! flush it to disk, then rename it, so a half-copied file is never visible
//! under the final name. The new file gets the original's permission bits
//! (and owner, where allowed) and, when asked, its modification/access times.
//! [`recover_artifact`] cleans up whatever a crash left behind.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use anyhow::Context as _;
use chrysopoeia_core::paths::{
    backup_file_name, is_artifact, is_backup, original_name_from_backup, temp_file_name,
};
use chrysopoeia_core::{Container, OutputMode};
use uuid::Uuid;

/// Longest file-name stem used for temp files, in bytes. Keeps temp names
/// under the usual 255-byte file-name limit.
const MAX_TEMP_STEM_BYTES: usize = 200;

/// Most filesystems limit a single file name to 255 bytes.
const MAX_FILE_NAME_BYTES: usize = 255;

/// Where the finished file will live.
///
/// - `Replace`: next to the input, with the container's extension.
/// - `Folder`: `output_folder` + the input's path relative to `library_root`,
///   with the container's extension. An input outside `library_root` lands
///   directly in `output_folder`. Without an `output_folder` the result is
///   the `Replace` path (the caller rejects that combination beforehand).
pub fn final_output_path(
    input: &Path,
    container: Container,
    mode: OutputMode,
    output_folder: Option<&Path>,
    library_root: &Path,
) -> PathBuf {
    let ext = container.extension();
    match (mode, output_folder) {
        (OutputMode::Folder, Some(folder)) => {
            let relative = input
                .strip_prefix(library_root)
                .ok()
                .filter(|rel| {
                    !rel.as_os_str().is_empty()
                        && rel.components().all(|c| matches!(c, Component::Normal(_)))
                })
                .map(Path::to_path_buf)
                .or_else(|| input.file_name().map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("output"));
            folder.join(relative).with_extension(ext)
        }
        _ => input.with_extension(ext),
    }
}

/// Hidden temp path for an in-progress encode, in `temp_dir` or next to the
/// input. Name from `chrysopoeia_core::paths::temp_file_name`; very long
/// names are shortened so the temp name stays valid.
pub fn temp_output_path(
    input: &Path,
    container: Container,
    job_id: Uuid,
    temp_dir: Option<&Path>,
) -> PathBuf {
    let dir = temp_dir
        .map(Path::to_path_buf)
        .or_else(|| input.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_string());
    let stem = truncate_to_bytes(&stem, MAX_TEMP_STEM_BYTES);
    dir.join(temp_file_name(stem, job_id, container.extension()))
}

/// Longest prefix of `s` that fits in `max` bytes without splitting a char.
fn truncate_to_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Inputs for [`finalize`].
#[derive(Debug, Clone, Copy)]
pub struct FinalizeRequest<'a> {
    /// The original file.
    pub input: &'a Path,
    /// The verified encode (see [`temp_output_path`]).
    pub temp: &'a Path,
    /// Where the result goes (see [`final_output_path`]).
    pub final_path: &'a Path,
    /// Replace the original, or write into a separate folder.
    pub mode: OutputMode,
    /// Names the backup and copy artifacts (see `chrysopoeia_core::paths`).
    pub job_id: Uuid,
    /// Copy the original's modification and access times onto the result.
    pub keep_dates: bool,
    /// Copy instead of renaming even on the same filesystem, exactly as a
    /// cross-device move does. Used by tests; normally false.
    pub force_copy: bool,
}

/// Move a verified encode into place. Returns the size of the final file.
///
/// On error the original is where it was (restored from the backup if
/// needed) and nothing has been written under the final name; the temp file
/// may still exist and is the caller's to delete.
pub async fn finalize(req: &FinalizeRequest<'_>) -> anyhow::Result<u64> {
    let owned = OwnedRequest {
        input: req.input.to_path_buf(),
        temp: req.temp.to_path_buf(),
        final_path: req.final_path.to_path_buf(),
        mode: req.mode,
        job_id: req.job_id,
        keep_dates: req.keep_dates,
        force_copy: req.force_copy,
    };
    tokio::task::spawn_blocking(move || finalize_blocking(&owned))
        .await
        .context("The file move was interrupted")?
}

#[derive(Debug)]
struct OwnedRequest {
    input: PathBuf,
    temp: PathBuf,
    final_path: PathBuf,
    mode: OutputMode,
    job_id: Uuid,
    keep_dates: bool,
    force_copy: bool,
}

fn finalize_blocking(req: &OwnedRequest) -> anyhow::Result<u64> {
    let original = fs::metadata(&req.input).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            anyhow::anyhow!("The original file is no longer there")
        } else {
            anyhow::anyhow!("Could not read the original file: {e}")
        }
    })?;
    let temp_meta = fs::metadata(&req.temp).map_err(|e| {
        anyhow::anyhow!(
            "The converted file is missing ({}): {e}",
            req.temp.display()
        )
    })?;
    if !temp_meta.is_file() || temp_meta.len() == 0 {
        anyhow::bail!("The converted file is empty");
    }

    // Make sure the encode is on disk before any name points at it, and give
    // it the original's permissions (and dates) so the rename publishes a
    // finished file in one step.
    sync_file(&req.temp);
    apply_metadata(&req.temp, &original, req.keep_dates);

    let mover = Mover {
        job_id: req.job_id,
        force_copy: req.force_copy,
        original: &original,
        keep_dates: req.keep_dates,
    };

    match req.mode {
        OutputMode::Replace if is_same_file(&req.input, &req.final_path) => {
            replace_in_place(req, &mover)?
        }
        OutputMode::Replace => replace_with_new_name(req, &mover)?,
        OutputMode::Folder => place_in_folder(req, &mover)?,
    }

    // The file is in place at this point; never report a failure from here.
    Ok(fs::metadata(&req.final_path)
        .map(|m| m.len())
        .unwrap_or(temp_meta.len()))
}

/// Same path: original → backup, new → final, delete backup.
fn replace_in_place(req: &OwnedRequest, mover: &Mover<'_>) -> anyhow::Result<()> {
    let dir = parent_dir(&req.final_path);
    let file_name = file_name_lossy(&req.input);
    let backup_name = backup_file_name(&file_name, req.job_id);
    if backup_name.len() > MAX_FILE_NAME_BYTES {
        // The backup name would be too long for the filesystem. A rename over
        // the original is atomic, so the original is never missing either way.
        mover
            .move_file(&req.temp, &req.final_path)
            .context("Could not put the new file in place")?;
        sync_dir(&dir);
        return Ok(());
    }
    let backup = dir.join(backup_name);

    fs::rename(&req.input, &backup)
        .map_err(|e| anyhow::anyhow!("Could not move the original aside to replace it: {e}"))?;

    if let Err(move_err) = mover.move_file(&req.temp, &req.final_path) {
        return match fs::rename(&backup, &req.input) {
            Ok(()) => Err(anyhow::anyhow!(
                "Could not put the new file in place, so the original was kept: {move_err}"
            )),
            Err(restore_err) => Err(anyhow::anyhow!(
                "Could not put the new file in place ({move_err}), and the original could not be \
                 moved back ({restore_err}). The original is safe at {}",
                backup.display()
            )),
        };
    }

    sync_dir(&dir);
    if let Err(e) = fs::remove_file(&backup) {
        // Harmless: the new file is in place, and startup recovery deletes
        // backups whose original exists.
        tracing::warn!(backup = %backup.display(), "could not delete the backup: {e}");
    }
    Ok(())
}

/// New extension: refuse to clobber, move in, then delete the original.
fn replace_with_new_name(req: &OwnedRequest, mover: &Mover<'_>) -> anyhow::Result<()> {
    if exists(&req.final_path)? {
        anyhow::bail!(
            "A file named \"{}\" already exists next to the original",
            file_name_lossy(&req.final_path)
        );
    }
    mover
        .move_file(&req.temp, &req.final_path)
        .context("Could not put the new file in place")?;
    sync_dir(&parent_dir(&req.final_path));
    if let Err(e) = fs::remove_file(&req.input) {
        tracing::warn!(
            input = %req.input.display(),
            "the new file is in place, but the original could not be deleted: {e}"
        );
    }
    Ok(())
}

/// Folder mode: create folders, refuse to clobber, never touch the input.
fn place_in_folder(req: &OwnedRequest, mover: &Mover<'_>) -> anyhow::Result<()> {
    let dir = parent_dir(&req.final_path);
    fs::create_dir_all(&dir)
        .with_context(|| format!("Could not create the folder {}", dir.display()))?;
    if exists(&req.final_path)? {
        anyhow::bail!(
            "A file named \"{}\" already exists in the output folder",
            file_name_lossy(&req.final_path)
        );
    }
    mover
        .move_file(&req.temp, &req.final_path)
        .context("Could not put the new file in the output folder")?;
    sync_dir(&dir);
    Ok(())
}

/// Moves files, falling back to copy + rename across filesystems.
struct Mover<'a> {
    job_id: Uuid,
    force_copy: bool,
    original: &'a fs::Metadata,
    keep_dates: bool,
}

impl Mover<'_> {
    /// Move `src` to `dst` (replacing `dst` if it exists). On error `dst` is
    /// unchanged and `src` still exists.
    fn move_file(&self, src: &Path, dst: &Path) -> io::Result<()> {
        if !self.force_copy {
            match fs::rename(src, dst) {
                Ok(()) => return Ok(()),
                Err(e) if is_cross_device(&e) => {
                    tracing::debug!("{} is on another filesystem; copying", dst.display());
                }
                Err(e) => return Err(e),
            }
        }
        self.copy_then_rename(src, dst)
    }

    /// Cross-device move: copy to a hidden name beside `dst`, flush, rename.
    fn copy_then_rename(&self, src: &Path, dst: &Path) -> io::Result<()> {
        let dir = parent_dir(dst);
        let stem = dst
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "output".to_string());
        let ext = dst
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_else(|| "bin".to_string());
        let staged = dir.join(temp_file_name(
            truncate_to_bytes(&stem, MAX_TEMP_STEM_BYTES),
            self.job_id,
            &format!("part.{ext}"),
        ));

        let result = (|| {
            fs::copy(src, &staged)?;
            fs::File::open(&staged)?.sync_all()?;
            apply_metadata(&staged, self.original, self.keep_dates);
            fs::rename(&staged, dst)
        })();
        if let Err(e) = result {
            if let Err(cleanup) = fs::remove_file(&staged) {
                if cleanup.kind() != io::ErrorKind::NotFound {
                    tracing::warn!(path = %staged.display(), "could not delete a partial copy: {cleanup}");
                }
            }
            return Err(e);
        }
        sync_dir(&dir);
        if let Err(e) = fs::remove_file(src) {
            tracing::warn!(path = %src.display(), "could not delete the temp file after copying: {e}");
        }
        Ok(())
    }
}

fn is_cross_device(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::CrossesDevices || e.raw_os_error() == Some(EXDEV)
}

/// `EXDEV` on Linux and macOS.
const EXDEV: i32 = 18;

/// Copy permission bits, owner (best effort) and optionally times from the
/// original onto `path`. Failures are logged, not fatal: the content is what
/// matters.
fn apply_metadata(path: &Path, original: &fs::Metadata, keep_dates: bool) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // Only root (or the owner, for the group) may do this; a container
        // running as the media owner already creates files with that owner.
        if let Err(e) = std::os::unix::fs::chown(path, Some(original.uid()), Some(original.gid())) {
            tracing::debug!(path = %path.display(), "could not copy the original's owner: {e}");
        }
    }
    if keep_dates {
        let atime = filetime::FileTime::from_last_access_time(original);
        let mtime = filetime::FileTime::from_last_modification_time(original);
        if let Err(e) = filetime::set_file_times(path, atime, mtime) {
            tracing::warn!(path = %path.display(), "could not copy the original's dates: {e}");
        }
    }
    // Permissions last: a read-only original must not stop the steps above.
    if let Err(e) = fs::set_permissions(path, original.permissions()) {
        tracing::warn!(path = %path.display(), "could not copy the original's permissions: {e}");
    }
}

/// Flush a file's data to disk. Errors are logged: some network filesystems
/// reject fsync, and that must not block the job.
fn sync_file(path: &Path) {
    if let Err(e) = fs::File::open(path).and_then(|f| f.sync_all()) {
        tracing::warn!(path = %path.display(), "could not flush the file to disk: {e}");
    }
}

/// Flush a directory entry change (rename) to disk, where supported.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Err(e) = fs::File::open(dir).and_then(|f| f.sync_all()) {
        tracing::debug!(dir = %dir.display(), "could not flush the folder to disk: {e}");
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn file_name_lossy(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Whether anything (file, folder or dangling link) exists at `path`.
fn exists(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(anyhow::anyhow!("Could not check {}: {e}", path.display())),
    }
}

/// Whether two paths name the same file: equal paths, or the same inode (a
/// case-insensitive filesystem where only the extension's case differs).
fn is_same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if let (Ok(ma), Ok(mb)) = (fs::metadata(a), fs::metadata(b)) {
            return ma.dev() == mb.dev() && ma.ino() == mb.ino();
        }
        false
    }
    #[cfg(not(unix))]
    {
        match (fs::canonicalize(a), fs::canonicalize(b)) {
            (Ok(ca), Ok(cb)) => ca == cb,
            _ => false,
        }
    }
}

/// Why [`finalize`] would refuse to write `final_path`, checked before any
/// encoding starts so a conflict fails in seconds rather than after hours.
/// `None` when the destination is free (or is the input itself in replace
/// mode). [`finalize`] checks again at the end.
pub async fn destination_conflict(
    input: &Path,
    final_path: &Path,
    mode: OutputMode,
) -> Option<String> {
    let input = input.to_path_buf();
    let final_path = final_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let taken = exists(&final_path).unwrap_or(false);
        match mode {
            OutputMode::Replace if taken && !is_same_file(&input, &final_path) => Some(format!(
                "A file named \"{}\" already exists next to the original",
                file_name_lossy(&final_path)
            )),
            OutputMode::Folder if taken => Some(format!(
                "A file named \"{}\" already exists in the output folder",
                file_name_lossy(&final_path)
            )),
            _ => None,
        }
    })
    .await
    .ok()
    .flatten()
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
    /// Not one of our files (or already gone); nothing was done.
    Untouched,
}

/// Clean up a temp or backup file left behind by a crash. Safe to call at
/// startup for every artifact the scanner reports.
///
/// - Temp files (`*.chrysopoeia-*.tmp.*`) are deleted.
/// - Backups (`*.chrysopoeia-*.bak`) are renamed back to the original name
///   when the original is missing, and deleted otherwise.
/// - Anything else (including folders) is left alone.
pub async fn recover_artifact(path: &Path) -> anyhow::Result<Recovery> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || recover_blocking(&path))
        .await
        .context("Recovery of a leftover file was interrupted")?
}

fn recover_blocking(path: &Path) -> anyhow::Result<Recovery> {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Ok(Recovery::Untouched);
    };
    if !is_artifact(name) {
        return Ok(Recovery::Untouched);
    }
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => return Ok(Recovery::Untouched),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Recovery::Untouched),
        Err(e) => return Err(anyhow::anyhow!("Could not read {}: {e}", path.display())),
    }

    if is_backup(name) {
        let Some(original_name) = original_name_from_backup(name) else {
            return Ok(Recovery::Untouched);
        };
        let original = path.with_file_name(original_name);
        return if exists(&original)? {
            fs::remove_file(path)
                .with_context(|| format!("Could not delete the backup {}", path.display()))?;
            Ok(Recovery::DeletedBackup)
        } else {
            fs::rename(path, &original).with_context(|| {
                format!("Could not restore {} from its backup", original.display())
            })?;
            sync_dir(&parent_dir(&original));
            Ok(Recovery::RestoredBackup(original))
        };
    }

    if name.contains(".tmp.") {
        fs::remove_file(path)
            .with_context(|| format!("Could not delete the temp file {}", path.display()))?;
        return Ok(Recovery::DeletedTemp);
    }
    Ok(Recovery::Untouched)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> Uuid {
        Uuid::parse_str("0123456789abcdef0123456789abcdef").unwrap()
    }

    #[test]
    fn replace_paths_change_only_the_extension() {
        let p = final_output_path(
            Path::new("/media/Movies/Film (2020).avi"),
            Container::Mkv,
            OutputMode::Replace,
            None,
            Path::new("/media"),
        );
        assert_eq!(p, Path::new("/media/Movies/Film (2020).mkv"));
        let same = final_output_path(
            Path::new("/media/a.b.c.mkv"),
            Container::Mkv,
            OutputMode::Replace,
            Some(Path::new("/out")),
            Path::new("/media"),
        );
        assert_eq!(same, Path::new("/media/a.b.c.mkv"));
    }

    #[test]
    fn folder_paths_mirror_the_library() {
        let p = final_output_path(
            Path::new("/media/TV/Show/S01E01.ts"),
            Container::Mp4,
            OutputMode::Folder,
            Some(Path::new("/out")),
            Path::new("/media"),
        );
        assert_eq!(p, Path::new("/out/TV/Show/S01E01.mp4"));
        // Outside the library root: straight into the output folder.
        let p = final_output_path(
            Path::new("/elsewhere/x.avi"),
            Container::Mkv,
            OutputMode::Folder,
            Some(Path::new("/out")),
            Path::new("/media"),
        );
        assert_eq!(p, Path::new("/out/x.mkv"));
        // No folder configured: falls back to the replace path.
        let p = final_output_path(
            Path::new("/media/x.avi"),
            Container::Mkv,
            OutputMode::Folder,
            None,
            Path::new("/media"),
        );
        assert_eq!(p, Path::new("/media/x.mkv"));
    }

    #[test]
    fn temp_paths_are_hidden_artifacts() {
        let t = temp_output_path(
            Path::new("/media/Movie (2020).avi"),
            Container::Mkv,
            id(),
            None,
        );
        assert_eq!(
            t,
            Path::new("/media/.Movie (2020).chrysopoeia-01234567.tmp.mkv")
        );
        let t = temp_output_path(
            Path::new("/media/Movie.avi"),
            Container::Webm,
            id(),
            Some(Path::new("/temp")),
        );
        assert_eq!(t, Path::new("/temp/.Movie.chrysopoeia-01234567.tmp.webm"));
    }

    #[test]
    fn long_names_are_shortened_for_temp_files() {
        let long = format!("/media/{}.mkv", "é".repeat(150));
        let t = temp_output_path(Path::new(&long), Container::Mkv, id(), None);
        let name = t.file_name().unwrap().to_str().unwrap();
        assert!(name.len() <= MAX_FILE_NAME_BYTES, "{} bytes", name.len());
        assert!(is_artifact(name));
    }

    #[test]
    fn cross_device_errors_are_recognised() {
        assert!(is_cross_device(&io::Error::from_raw_os_error(EXDEV)));
        assert!(!is_cross_device(&io::Error::from(io::ErrorKind::NotFound)));
    }
}
