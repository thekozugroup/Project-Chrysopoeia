//! Output paths and crash-safe replacement of originals.
//!
//! The encoder writes to a hidden temp file ([`temp_output_path`]). Only after
//! verification does [`finalize`] move it to its final place
//! ([`final_output_path`]). The new file is first staged under a hidden name
//! in the destination folder (a rename, or, across filesystems, a copy that is
//! flushed to disk), so the original stays in the library until the new file
//! is completely there. Then:
//!
//! - Replace mode, same path: the original is renamed to a hidden backup in
//!   its own folder, the new file is renamed in, and the backup is deleted.
//!   Any failure puts the backup back.
//! - Replace mode, new extension: refuses to overwrite an unrelated file;
//!   the original is renamed to a hidden backup, the new file is renamed in
//!   under its new name, and the backup is deleted.
//! - Folder mode: creates the folders, refuses to overwrite anything and never
//!   touches the original.
//!
//! A new name (folder mode, or a new extension) is claimed for the whole
//! step: two jobs of this process aiming at one name (two libraries holding
//! the same relative path, saved to one output folder) can't both find it
//! free, so the second is refused instead of overwriting the first's file.
//!
//! If the original no longer matches the [`FileIdentity`] recorded when the
//! job started (a Sonarr/Radarr upgrade replaced it mid-encode), nothing is
//! replaced and [`OriginalChanged`] is returned. The new file gets the
//! original's permission bits (and owner, where allowed) and, when asked, its
//! modification/access times. After a crash, [`resume_replace`] tells
//! whether a replacement's new file was already in place (the caller then
//! records that and removes the backup with [`remove_backup`]), and
//! [`recover_artifact`] cleans up whatever else was left behind.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::Context as _;
use chrysopoeia_core::paths::{
    backup_file_name, is_artifact, is_backup, original_name_from_backup, temp_file_name,
};
use chrysopoeia_core::plain::io_reason;
use chrysopoeia_core::{Container, OutputMode, ProblemKind};
use uuid::Uuid;

use crate::slow_fs::NotAnswering;

/// Longest file-name stem used for temp files, in bytes. Keeps temp names
/// under the usual 255-byte file-name limit.
const MAX_TEMP_STEM_BYTES: usize = 200;

/// Most filesystems limit a single file name to 255 bytes.
const MAX_FILE_NAME_BYTES: usize = 255;

/// Free space left over after a cross-filesystem copy, on top of 1 %.
const COPY_MARGIN_BYTES: u64 = 16 * 1024 * 1024;

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

/// What identifies one version of a file: its size and modification time.
///
/// Recorded when a job starts and compared before the original is replaced,
/// so a file that was swapped for a newer release mid-encode (Sonarr/Radarr
/// upgrades) is never overwritten with a conversion of the old one. Inode
/// numbers are deliberately left out: Unraid's user shares (FUSE) may hand
/// out new ones for the same file, and the mover keeps size and dates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    len: u64,
    modified: filetime::FileTime,
}

impl FileIdentity {
    /// The identity of a file from its metadata.
    pub fn of(meta: &fs::Metadata) -> Self {
        Self {
            len: meta.len(),
            modified: filetime::FileTime::from_last_modification_time(meta),
        }
    }

    /// The identity of the file at `path` now.
    pub async fn read(path: &Path) -> io::Result<Self> {
        tokio::fs::metadata(path).await.map(|m| Self::of(&m))
    }

    /// Size in bytes.
    pub fn len(&self) -> u64 {
        self.len
    }

    /// Whether the file is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// The original was replaced or modified while the job ran, so it was left
/// alone. [`finalize`] returns this (inside its `anyhow::Error`) so callers
/// can report a skip rather than a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OriginalChanged;

impl std::fmt::Display for OriginalChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The original changed while it was being converted, so it was left alone")
    }
}

impl std::error::Error for OriginalChanged {}

/// Putting the new file in place was stopped (see [`start_finalize`])
/// before the new file took the original's place: everything it had done
/// was undone, so the original is where it was. [`finalize`] returns this
/// inside its `anyhow::Error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Undone;

impl std::fmt::Display for Undone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Stopped before the new file was put in place, so nothing was changed")
    }
}

impl std::error::Error for Undone {}

/// Why [`finalize`] couldn't put the new file in place: one or two plain
/// sentences saying what happened and what to do, and what kind of problem
/// it is. [`finalize`] returns this inside its `anyhow::Error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceError {
    /// What kind of problem it is.
    pub problem: ProblemKind,
    /// What happened and what to do.
    pub message: String,
}

impl PlaceError {
    fn error(problem: ProblemKind, message: impl Into<String>) -> anyhow::Error {
        Self {
            problem,
            message: message.into(),
        }
        .into()
    }
}

impl std::fmt::Display for PlaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PlaceError {}

/// How messages name the folder a converted file goes to (`dir`): "the
/// output folder /out/TV" or "the original's folder /media/Films", so a
/// path never appears without saying what it is.
pub(crate) fn destination_name(dir: &Path, mode: OutputMode) -> String {
    match mode {
        OutputMode::Folder => format!("the output folder {}", dir.display()),
        OutputMode::Replace => format!("the original's folder {}", dir.display()),
    }
}

/// The error for a file operation that failed while the new file was being
/// put into `dir` (in `mode`). The original is where it was.
fn placing_failed(e: &io::Error, dir: &Path, mode: OutputMode) -> anyhow::Error {
    use io::ErrorKind as K;
    let folder = destination_name(dir, mode);
    let other_place = match mode {
        OutputMode::Folder => "choose another output folder in Settings › Output",
        OutputMode::Replace => "save converted files to a separate folder in Settings › Output",
    };
    match e.kind() {
        K::StorageFull | K::QuotaExceeded => {
            // A copy refused up front says how big the new file is.
            let size = e
                .get_ref()
                .map(|inner| format!(" ({inner})"))
                .unwrap_or_default();
            let fix = match mode {
                OutputMode::Folder => {
                    "Free up some space there, or choose another output folder in Settings › \
                     Output."
                }
                OutputMode::Replace => "Free up some space on that disk, then try again.",
            };
            PlaceError::error(
                ProblemKind::DiskFull,
                format!(
                    "There isn't enough free space in {folder} for the new file{size}, so the \
                     original was kept. {fix}"
                ),
            )
        }
        K::PermissionDenied => PlaceError::error(
            ProblemKind::Destination,
            format!(
                "Chrysopoeia doesn't have permission to write in {folder}, so the new file \
                 couldn't be put there and the original was kept. Check the folder's \
                 permissions (in Docker, the PUID/PGID user needs write access), or \
                 {other_place}."
            ),
        ),
        K::ReadOnlyFilesystem => PlaceError::error(
            ProblemKind::Destination,
            format!(
                "{} is on a read-only drive, so the new file couldn't be put there and the \
                 original was kept. Make the drive writable, or {other_place}.",
                crate::run::capitalize_first(&folder)
            ),
        ),
        _ => PlaceError::error(
            ProblemKind::Destination,
            format!(
                "The new file couldn't be put in {folder} because {}, so the original was kept. \
                 Check that folder, then try again.",
                io_reason(e)
            ),
        ),
    }
}

/// The error when the original is gone or can't be read right before it
/// would be replaced.
fn original_unavailable(e: &io::Error) -> anyhow::Error {
    if e.kind() == io::ErrorKind::NotFound {
        PlaceError::error(
            ProblemKind::SourceChanged,
            "The original is no longer there, so the new file wasn't put in place. It may have \
             been moved or deleted while it was being converted. If it was moved, scan the \
             library again to find it.",
        )
    } else {
        PlaceError::error(
            ProblemKind::UnreadableSource,
            format!(
                "The original couldn't be checked before replacing it because {}, so nothing \
                 was changed.",
                io_reason(e)
            ),
        )
    }
}

/// The error when the new file couldn't take the original's place and the
/// original couldn't be moved back from its backup either.
fn restore_failed(backup: &Path, original: &Path, e: &io::Error) -> anyhow::Error {
    PlaceError::error(
        ProblemKind::Destination,
        format!(
            "The new file couldn't be put in place, and the original couldn't be moved back to \
             its name because {}. The original is safe as the hidden file \"{}\" in {}: rename \
             it back to \"{}\".",
            io_reason(e),
            file_name_lossy(backup),
            parent_dir(backup).display(),
            file_name_lossy(original)
        ),
    )
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
    /// The original as it was when the encode started. When set, finalize
    /// refuses with [`OriginalChanged`] if the file no longer matches.
    pub original: Option<FileIdentity>,
    /// Copy instead of renaming even on the same filesystem, exactly as a
    /// cross-device move does. Used by tests; normally false.
    pub force_copy: bool,
}

/// What [`finalize`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finalized {
    /// Size of the file now at the final path, in bytes.
    pub size: u64,
    /// Plain-language notes about anything that did not go fully to plan
    /// but did not stop the job, e.g. an original that could not be deleted.
    pub notes: Vec<String>,
}

/// Move a verified encode into place.
///
/// The new file is first staged beside its destination (renamed there, or
/// copied and flushed when the temp folder is on another filesystem); the
/// original is only touched after that, so it never disappears from the
/// library during a long copy. Then, per mode:
///
/// - Replace, same path: original → hidden backup, staged → final, backup
///   deleted. A failure puts the backup back.
/// - Replace, new extension: original → hidden backup, staged → final
///   (never over an unrelated file), backup deleted. A failure puts the
///   backup back.
/// - Folder: staged → final (never over an existing file); the original is
///   left alone.
///
/// On error the original is where it was (restored from the backup if
/// needed), nothing has been written under the final name, and no staged
/// copy is left behind; the temp file may still exist and is the caller's to
/// delete. If the original no longer matches `req.original` the error is
/// [`OriginalChanged`].
pub async fn finalize(req: &FinalizeRequest<'_>) -> anyhow::Result<Finalized> {
    joined(start_finalize(req, Arc::new(AtomicBool::new(false))).await)
}

/// [`finalize`] on its own blocking thread, which goes on even when the
/// returned handle is dropped: a rename on a share that stopped answering
/// can't be called back once it has started, and leaving it half done
/// would leave the files out of step with what is recorded about them.
///
/// Setting `stop` asks it to stop at the next safe point: before the
/// original is touched, or right after it was moved aside (it is put back
/// then). Stopped there, it undoes what it did and fails with [`Undone`];
/// once the new file has taken its final name, it finishes. Pass the
/// handle's result to [`joined`].
pub fn start_finalize(
    req: &FinalizeRequest<'_>,
    stop: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<anyhow::Result<Finalized>> {
    let owned = OwnedRequest {
        input: req.input.to_path_buf(),
        temp: req.temp.to_path_buf(),
        final_path: req.final_path.to_path_buf(),
        mode: req.mode,
        job_id: req.job_id,
        keep_dates: req.keep_dates,
        original: req.original,
        force_copy: req.force_copy,
        stop,
    };
    tokio::task::spawn_blocking(move || finalize_blocking(&owned))
}

/// The result of a [`start_finalize`] thread.
pub fn joined(
    result: Result<anyhow::Result<Finalized>, tokio::task::JoinError>,
) -> anyhow::Result<Finalized> {
    result.map_err(|e| {
        tracing::warn!("putting a new file in place stopped unexpectedly: {e}");
        PlaceError::error(
            ProblemKind::Other,
            "Putting the new file in place stopped unexpectedly. The details are in the \
             server log.",
        )
    })?
}

#[derive(Debug)]
struct OwnedRequest {
    input: PathBuf,
    temp: PathBuf,
    final_path: PathBuf,
    mode: OutputMode,
    job_id: Uuid,
    keep_dates: bool,
    original: Option<FileIdentity>,
    force_copy: bool,
    /// Asked to stop at the next safe point (see [`start_finalize`]).
    stop: Arc<AtomicBool>,
}

impl OwnedRequest {
    /// Fail with [`Undone`] when asked to stop (nothing has been changed
    /// yet, or the caller undoes it first).
    fn check_stop(&self) -> anyhow::Result<()> {
        if self.stop.load(Ordering::SeqCst) {
            tracing::debug!(job = %self.job_id, "stopped before the new file was put in place");
            return Err(Undone.into());
        }
        Ok(())
    }

    /// Fail with [`OriginalChanged`] when the input no longer matches the
    /// identity recorded at the start of the job.
    fn check_original(&self) -> anyhow::Result<()> {
        let Some(expected) = self.original else {
            return Ok(());
        };
        match fs::metadata(&self.input) {
            Ok(meta) if FileIdentity::of(&meta) == expected => Ok(()),
            Ok(_) => Err(OriginalChanged.into()),
            Err(e) => Err(original_unavailable(&e)),
        }
    }
}

fn finalize_blocking(req: &OwnedRequest) -> anyhow::Result<Finalized> {
    hold::pause(req.job_id, hold::Step::Start);
    req.check_stop()?;
    let original = fs::metadata(&req.input).map_err(|e| original_unavailable(&e))?;
    if req
        .original
        .is_some_and(|expected| FileIdentity::of(&original) != expected)
    {
        return Err(OriginalChanged.into());
    }
    let temp_meta = fs::metadata(&req.temp).map_err(|e| {
        tracing::warn!(temp = %req.temp.display(), "the converted file is gone: {e}");
        PlaceError::error(
            ProblemKind::Other,
            "The converted file disappeared before it could be put in place, so the original \
             was kept. Try again.",
        )
    })?;
    if !temp_meta.is_file() || temp_meta.len() == 0 {
        return Err(PlaceError::error(
            ProblemKind::Other,
            "The converted file turned out empty, so the original was kept. Try again.",
        ));
    }

    // Make sure the encode is on disk before any name points at it, and give
    // it the original's permissions (and dates) so the rename publishes a
    // finished file in one step.
    sync_file(&req.temp);
    apply_metadata(&req.temp, &original, req.keep_dates);
    req.check_stop()?;

    let mover = Mover {
        job_id: req.job_id,
        force_copy: req.force_copy,
        original: &original,
        keep_dates: req.keep_dates,
        #[cfg(test)]
        hooks: TestHooks::default(),
    };

    let mut notes = Vec::new();
    match req.mode {
        OutputMode::Replace if is_same_file(&req.input, &req.final_path) => {
            replace_in_place(req, &mover)?
        }
        OutputMode::Replace => replace_with_new_name(req, &mover, &mut notes)?,
        OutputMode::Folder => place_in_folder(req, &mover)?,
    }

    // The file is in place at this point; never report a failure from here.
    let placed = fs::metadata(&req.final_path);
    let size = placed.as_ref().map_or(temp_meta.len(), fs::Metadata::len);
    #[cfg(unix)]
    if let Ok(placed) = &placed {
        use std::os::unix::fs::MetadataExt as _;
        notes.extend(ownership_note(
            (original.uid(), original.gid()),
            (placed.uid(), placed.gid()),
        ));
    }
    Ok(Finalized { size, notes })
}

/// A job note when the new file couldn't keep the original's owner or group
/// (`(user, group)` ids): with group-based access, the media server may not
/// be able to read it.
fn ownership_note(original: (u32, u32), new: (u32, u32)) -> Option<String> {
    let ((user, group), (new_user, new_group)) = (original, new);
    let what = match (user == new_user, group == new_group) {
        (true, true) => return None,
        (false, true) => format!(
            "The new file couldn't keep the original's owner (user {user}), so it belongs to \
             user {new_user}"
        ),
        (true, false) => format!(
            "The new file couldn't keep the original's group (group {group}), so it is in group \
             {new_group}"
        ),
        (false, false) => format!(
            "The new file couldn't keep the original's owner and group (user {user}, group \
             {group}), so it belongs to user {new_user} and group {new_group}"
        ),
    };
    Some(format!(
        "{what}. If your media server can't open it, run Chrysopoeia as the owner of your media \
         (PUID and PGID)"
    ))
}

/// Same path: stage beside the original, then original → backup,
/// staged → final, delete backup.
fn replace_in_place(req: &OwnedRequest, mover: &Mover<'_>) -> anyhow::Result<()> {
    let dir = parent_dir(&req.final_path);
    let staged = mover
        .stage(&req.temp, &req.final_path)
        .map_err(|e| placing_failed(&e, &dir, req.mode))?;
    let result = swap_in(req, mover, &staged, &dir, None, &mut Vec::new());
    if result.is_err() {
        mover.discard(&staged, &req.temp);
    }
    result
}

/// Original → hidden backup, staged → final, backup deleted. The backup is
/// what makes a crash between the steps recoverable (see
/// [`resume_replace`]): while it exists, the original is safe, and a backup
/// next to a finished new file means the new file is complete.
///
/// `conflict` is set for a new file name: the final name must still be free
/// when the new file moves in.
fn swap_in(
    req: &OwnedRequest,
    mover: &Mover<'_>,
    staged: &Path,
    dir: &Path,
    conflict: Option<&dyn Fn() -> anyhow::Error>,
    notes: &mut Vec<String>,
) -> anyhow::Result<()> {
    let taken = || -> anyhow::Result<()> {
        match conflict {
            Some(conflict) if exists(&req.final_path)? => Err(conflict()),
            _ => Ok(()),
        }
    };
    // A cross-device copy can take minutes; look again right before the swap.
    req.check_stop()?;
    req.check_original()?;
    taken()?;
    let file_name = file_name_lossy(&req.input);
    let backup_name = backup_file_name(&file_name, req.job_id);
    if backup_name.len() > MAX_FILE_NAME_BYTES {
        // The backup name would be too long for the filesystem.
        return commit_without_backup(req, mover, staged, dir, notes);
    }
    let backup = dir.join(backup_name);

    req.check_stop()?;
    fs::rename(&req.input, &backup).map_err(|e| placing_failed(&e, dir, req.mode))?;
    hold::pause(req.job_id, hold::Step::MovedAside);

    // What was moved aside must be the file the job read. Checked on the
    // backup itself, so a replacement that raced the rename is caught too.
    if let Some(expected) = req.original {
        let moved = fs::metadata(&backup).map(|m| FileIdentity::of(&m));
        if moved.as_ref().ok() != Some(&expected) {
            restore_backup(&backup, &req.input)
                .map_err(|e| restore_failed(&backup, &req.input, &e))?;
            return Err(OriginalChanged.into());
        }
    }

    // Asked to stop (a rename that waited for a share that stopped
    // answering has just finished): put the original back.
    let committed = match req.check_stop().and_then(|()| taken()) {
        Err(problem) => Err(problem),
        Ok(()) => mover
            .commit(staged, &req.final_path, &req.temp)
            .map_err(|e| placing_failed(&e, dir, req.mode)),
    };
    if let Err(problem) = committed {
        restore_backup(&backup, &req.input).map_err(|e| restore_failed(&backup, &req.input, &e))?;
        return Err(problem);
    }
    // The new file has its name: from here on, finish.
    hold::pause(req.job_id, hold::Step::Committed);
    sync_dir(dir);

    // New name: a file that appeared under the original's name meanwhile
    // (an upgrade landing at that very moment) is newer than what was
    // converted, so the conversion is taken out again.
    if conflict.is_some() && exists(&req.input).unwrap_or(false) {
        remove_quietly(&req.final_path);
        remove_quietly(&backup);
        sync_dir(dir);
        return Err(OriginalChanged.into());
    }

    if let Err(e) = fs::remove_file(&backup) {
        // Harmless for a replacement under the same name: startup recovery
        // deletes backups whose original exists.
        tracing::warn!(backup = %backup.display(), "could not delete the backup: {e}");
        if conflict.is_some() {
            notes.push(format!(
                "The new file is in place, but the original \"{file_name}\" couldn't be \
                 deleted because {}. It is kept as the hidden file \"{}\"",
                io_reason(&e),
                file_name_lossy(&backup)
            ));
        }
    }
    Ok(())
}

/// Put the new file in place when the backup name would be too long for
/// the filesystem. Same name: a rename over the original is atomic, so the
/// original is never missing. New name: the new file goes in first, then
/// the original is deleted.
fn commit_without_backup(
    req: &OwnedRequest,
    mover: &Mover<'_>,
    staged: &Path,
    dir: &Path,
    notes: &mut Vec<String>,
) -> anyhow::Result<()> {
    req.check_stop()?;
    mover
        .commit(staged, &req.final_path, &req.temp)
        .map_err(|e| placing_failed(&e, dir, req.mode))?;
    hold::pause(req.job_id, hold::Step::Committed);
    sync_dir(dir);
    if req.input == req.final_path || is_same_file(&req.input, &req.final_path) {
        return Ok(());
    }
    // The original changed after the new file went in: the new file is a
    // conversion of the old version, so take it out again.
    if let Err(e) = req.check_original()
        && e.is::<OriginalChanged>()
    {
        remove_quietly(&req.final_path);
        return Err(e);
    }
    if let Err(e) = fs::remove_file(&req.input) {
        tracing::warn!(
            input = %req.input.display(),
            "the new file is in place, but the original could not be deleted: {e}"
        );
        notes.push(format!(
            "The new file is in place, but the original \"{}\" couldn't be deleted because {}",
            file_name_lossy(&req.input),
            io_reason(&e)
        ));
    }
    Ok(())
}

/// Put a backup back under the original name.
fn restore_backup(backup: &Path, original: &Path) -> io::Result<()> {
    fs::rename(backup, original)?;
    sync_dir(&parent_dir(original));
    Ok(())
}

/// New extension: refuse to clobber, stage, then original → backup,
/// staged → final, backup deleted (see [`swap_in`]).
fn replace_with_new_name(
    req: &OwnedRequest,
    mover: &Mover<'_>,
    notes: &mut Vec<String>,
) -> anyhow::Result<()> {
    let conflict = || {
        PlaceError::error(
            ProblemKind::Destination,
            format!(
                "A file named \"{}\" appeared next to the original while it was being \
                 converted, so the new file wasn't put in place. Move or rename that file, then \
                 convert this one again.",
                file_name_lossy(&req.final_path)
            ),
        )
    };
    // Another job putting a file under this name right now (another
    // original with the same name and another extension): its file wins.
    let Some(_claim) = NameClaim::take(&req.final_path) else {
        return Err(conflict());
    };
    if exists(&req.final_path)? {
        return Err(conflict());
    }
    let dir = parent_dir(&req.final_path);
    let staged = mover
        .stage(&req.temp, &req.final_path)
        .map_err(|e| placing_failed(&e, &dir, req.mode))?;
    let result = swap_in(req, mover, &staged, &dir, Some(&conflict), notes);
    if result.is_err() {
        mover.discard(&staged, &req.temp);
    }
    result
}

/// New names being put in place by this process right now (see
/// [`NameClaim`]).
static NAMES_BEING_PLACED: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// A new file's final name, claimed while it is put in place, so that two
/// jobs aiming at the same name (two libraries holding the same relative
/// path, saved to one output folder) can't both find it free and the second
/// rename overwrite the first's file. Released when dropped.
struct NameClaim(PathBuf);

impl NameClaim {
    /// Claim `path`; `None` when another job is putting a file there now.
    fn take(path: &Path) -> Option<Self> {
        NAMES_BEING_PLACED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.to_path_buf())
            .then(|| Self(path.to_path_buf()))
    }
}

impl Drop for NameClaim {
    fn drop(&mut self) {
        NAMES_BEING_PLACED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.0);
    }
}

/// Start of the errors for a converted file whose name in the output folder
/// is taken (see [`output_name_taken`]).
const NAME_TAKEN_START: &str = "A file named \"";

/// Whether a job's error says that the converted file's name in the output
/// folder was already taken (before the encode, or by a file that appeared
/// while it ran), so nothing was written there.
pub fn output_name_taken(error: &str) -> bool {
    error.starts_with(NAME_TAKEN_START)
        && (error.contains("\" is already in the output folder, so it wasn't overwritten")
            || error.contains("\" appeared in the output folder while this file was being"))
}

/// The error when a file took the new file's name in the output folder
/// while it was being converted.
fn appeared_in_output_folder(final_path: &Path) -> anyhow::Error {
    PlaceError::error(
        ProblemKind::Destination,
        format!(
            "{NAME_TAKEN_START}{}\" appeared in the output folder while this file was being \
             converted, so it wasn't overwritten. Move or delete it, then convert this file \
             again.",
            file_name_lossy(final_path)
        ),
    )
}

/// Folder mode: create folders, refuse to clobber, never touch the input.
fn place_in_folder(req: &OwnedRequest, mover: &Mover<'_>) -> anyhow::Result<()> {
    let conflict = || appeared_in_output_folder(&req.final_path);
    // Another job putting a file under this name right now: its file wins.
    let Some(_claim) = NameClaim::take(&req.final_path) else {
        return Err(conflict());
    };
    let dir = parent_dir(&req.final_path);
    fs::create_dir_all(&dir).map_err(|e| placing_failed(&e, &dir, req.mode))?;
    if exists(&req.final_path)? {
        return Err(conflict());
    }
    let staged = mover
        .stage(&req.temp, &req.final_path)
        .map_err(|e| placing_failed(&e, &dir, req.mode))?;
    let placed = (|| {
        req.check_stop()?;
        req.check_original()?;
        if exists(&req.final_path)? {
            return Err(conflict());
        }
        mover
            .commit(&staged, &req.final_path, &req.temp)
            .map_err(|e| placing_failed(&e, &dir, req.mode))
    })();
    if let Err(e) = placed {
        mover.discard(&staged, &req.temp);
        return Err(e);
    }
    hold::pause(req.job_id, hold::Step::Committed);
    sync_dir(&dir);
    Ok(())
}

/// Moves files into their destination folder, copying across filesystems.
struct Mover<'a> {
    job_id: Uuid,
    force_copy: bool,
    original: &'a fs::Metadata,
    keep_dates: bool,
    #[cfg(test)]
    hooks: TestHooks<'a>,
}

/// Fault injection and observation points for the unit tests.
#[cfg(test)]
#[derive(Default)]
struct TestHooks<'a> {
    /// Called once the new file is staged, before the original is touched.
    after_stage: Option<&'a dyn Fn(&Path)>,
    /// Make the final rename fail.
    fail_commit: bool,
    /// Called right before the final rename, with the final path.
    before_commit: Option<&'a dyn Fn(&Path)>,
}

impl Mover<'_> {
    /// Hidden name for the new file in `dst`'s folder while it is staged.
    fn staged_path(&self, dst: &Path) -> PathBuf {
        let stem = dst
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "output".to_string());
        let ext = dst
            .extension()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_else(|| "bin".to_string());
        parent_dir(dst).join(temp_file_name(
            truncate_to_bytes(&stem, MAX_TEMP_STEM_BYTES),
            self.job_id,
            &format!("part.{ext}"),
        ))
    }

    /// Bring `src` into `dst`'s folder under a hidden name, so the final step
    /// is a rename within one folder. A temp file already in that folder is
    /// used as it is; one elsewhere is renamed over, or copied and flushed
    /// to disk when it is on another filesystem. Nothing visible changes.
    fn stage(&self, src: &Path, dst: &Path) -> io::Result<PathBuf> {
        let staged = if parent_dir(src) == parent_dir(dst) && !self.force_copy {
            src.to_path_buf()
        } else {
            let staged = self.staged_path(dst);
            let renamed = if self.force_copy {
                false
            } else {
                match fs::rename(src, &staged) {
                    Ok(()) => true,
                    Err(e) if is_cross_device(&e) => {
                        tracing::debug!("{} is on another filesystem; copying", dst.display());
                        false
                    }
                    Err(e) => return Err(e),
                }
            };
            if !renamed {
                self.copy_to(src, &staged)?;
            }
            staged
        };
        #[cfg(test)]
        if let Some(hook) = self.hooks.after_stage {
            hook(&staged);
        }
        Ok(staged)
    }

    /// Copy `src` to `staged` and flush it; on failure nothing is left.
    /// Refuses up front when the destination cannot hold the copy, rather
    /// than filling the disk.
    fn copy_to(&self, src: &Path, staged: &Path) -> io::Result<()> {
        let size = fs::metadata(src)?.len();
        let needed = size
            .saturating_add(size / 100)
            .saturating_add(COPY_MARGIN_BYTES);
        let dir = parent_dir(staged);
        if let Some((_, free)) = filesystem_of(&dir)
            && free < needed
        {
            // The message gives the file's size; the margin is ours.
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                crate::validate::human_bytes(size),
            ));
        }
        let result = (|| {
            fs::copy(src, staged)?;
            fs::File::open(staged)?.sync_all()?;
            apply_metadata(staged, self.original, self.keep_dates);
            Ok(())
        })();
        if result.is_err() {
            remove_quietly(staged);
        }
        result
    }

    /// Rename the staged file to its final name (same folder), then remove
    /// the temp file if the staged file was a copy of it.
    fn commit(&self, staged: &Path, dst: &Path, temp: &Path) -> io::Result<()> {
        #[cfg(test)]
        {
            if let Some(hook) = self.hooks.before_commit {
                hook(dst);
            }
            if self.hooks.fail_commit {
                return Err(io::Error::other("injected failure"));
            }
        }
        fs::rename(staged, dst)?;
        if staged != temp {
            remove_quietly(temp);
        }
        Ok(())
    }

    /// Remove a staged file after a failure (a temp file used in place is
    /// left for the caller, like any other temp file).
    fn discard(&self, staged: &Path, temp: &Path) {
        if staged != temp {
            remove_quietly(staged);
        }
    }
}

/// Delete a file, logging anything but "already gone".
fn remove_quietly(path: &Path) {
    if let Err(e) = fs::remove_file(path)
        && e.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), "could not delete a leftover file: {e}");
    }
}

/// Device id and free bytes (for unprivileged users) of the filesystem
/// holding `dir`, or of its nearest existing parent. `None` when unknown
/// (space checks are then skipped).
#[cfg(unix)]
pub(crate) fn filesystem_of(dir: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    let existing = dir.ancestors().find(|d| d.exists())?;
    let device = fs::metadata(existing).ok()?.dev();
    let stat = rustix::fs::statvfs(existing).ok()?;
    Some((device, stat.f_bavail.saturating_mul(stat.f_frsize)))
}

/// See the Unix version; not available elsewhere.
#[cfg(not(unix))]
pub(crate) fn filesystem_of(_dir: &Path) -> Option<(u64, u64)> {
    None
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
        // Only root may give a file away; a container running as the media
        // owner already creates files with that owner. The group alone is
        // often allowed (any group the process belongs to), and it is what
        // group-based access (0640 in group `media`) depends on. A result
        // that still differs gets a job note (see `ownership_note`).
        if let Err(e) = std::os::unix::fs::chown(path, Some(original.uid()), Some(original.gid())) {
            tracing::debug!(path = %path.display(), "could not copy the original's owner: {e}");
            if let Err(e) = std::os::unix::fs::chown(path, None, Some(original.gid())) {
                tracing::debug!(path = %path.display(), "could not copy the original's group: {e}");
            }
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

/// Longest file name most filesystems (ext4, XFS, Btrfs, ZFS) allow.
const NAME_MAX_BYTES: usize = 255;

/// Whether anything (file, folder or dangling link) exists at `path`.
fn exists(path: &Path) -> anyhow::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(PlaceError::error(
            ProblemKind::Destination,
            format!(
                "Chrysopoeia couldn't check {} because {}, so nothing was changed.",
                path.display(),
                io_reason(&e)
            ),
        )),
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
/// mode). [`finalize`] checks again at the end. Gives up after `timeout`
/// when the folder doesn't answer (see [`crate::slow_fs`]).
pub async fn destination_conflict(
    input: &Path,
    final_path: &Path,
    mode: OutputMode,
    timeout: Duration,
) -> Result<Option<String>, NotAnswering> {
    let key = final_path.to_path_buf();
    let input = input.to_path_buf();
    let final_path = final_path.to_path_buf();
    crate::slow_fs::guarded("destination_conflict", &key, timeout, move || {
        // A longer extension (.ts to .mkv) can push a long name past what
        // the disk allows; find out now, not after the whole encode.
        let name_bytes = final_path.file_name().map_or(0, |n| n.len());
        if name_bytes > NAME_MAX_BYTES {
            return Some(format!(
                "The converted file's name would be too long for the disk ({name_bytes} bytes, \
                 more than the {NAME_MAX_BYTES} a name can have), so this file wasn't \
                 converted. Give it a shorter name, then try again."
            ));
        }
        // A name that can't be checked is not a free name.
        let taken = match exists(&final_path) {
            Ok(taken) => taken,
            Err(e) => {
                return Some(
                    e.downcast_ref::<PlaceError>()
                        .map_or_else(|| e.to_string(), |p| p.message.clone()),
                );
            }
        };
        match mode {
            OutputMode::Replace if taken && !is_same_file(&input, &final_path) => Some(format!(
                "A file named \"{}\" is already next to the original, so the new file can't \
                 take its name. Move or rename that file, then try again.",
                file_name_lossy(&final_path)
            )),
            OutputMode::Folder if taken => Some(format!(
                "{NAME_TAKEN_START}{}\" is already in the output folder, so it wasn't \
                 overwritten. Move or delete it, then try again.",
                file_name_lossy(&final_path)
            )),
            _ => None,
        }
    })
    .await
    .map_err(|e| e.at(&key))
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

/// Whether a replacement interrupted by a crash had already put the new
/// file in place (see [`resume_replace`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Interrupted {
    /// The new file was in place. The leftover backup of the original is
    /// still there: once that is recorded, [`remove_backup`] completes the
    /// replacement.
    Placed {
        /// Size of the new file in bytes.
        size: u64,
        /// Size of the original (its backup) in bytes.
        original_size: u64,
    },
    /// The new file was not in place: the original is where it was, or its
    /// backup is still there for [`recover_artifact`] to put back.
    NotPlaced,
}

/// After a crash, find out whether job `job_id`'s replacement of `input`
/// by `final_path` (replace mode) had already put the new file in place.
/// Only looks: the backup of the original stays, so the answer can be
/// asked for again until it is recorded (a look whose caller gave up on a
/// share that stopped answering may still finish by itself); then
/// [`remove_backup`] completes the replacement.
///
/// The backup is the evidence, because [`finalize`] moves the original
/// aside before the new file takes its final name and deletes the backup
/// last: a backup together with a file under the final name (the original's
/// own name, or, for a new extension, the new name while the original's
/// name is free) means the new file, flushed to disk before it was
/// renamed, is complete. Without a backup nothing is claimed.
///
/// Call it before [`recover_artifact`], which would put such a backup back
/// next to the new file.
///
/// Gives up after `timeout` when the folder doesn't answer: the error is
/// then a [`NotAnswering`].
pub async fn resume_replace(
    input: &Path,
    final_path: &Path,
    job_id: Uuid,
    timeout: Duration,
) -> anyhow::Result<Interrupted> {
    let key = parent_dir(input).join(job_id.to_string());
    let input = input.to_path_buf();
    let final_path = final_path.to_path_buf();
    let checked = crate::slow_fs::guarded("resume_replace", &key, timeout, move || {
        resume_blocking(&input, &final_path, job_id).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.at(parent_dir(&key).as_path()))?;
    checked.map_err(|e| anyhow::anyhow!(e))
}

fn resume_blocking(input: &Path, final_path: &Path, job_id: Uuid) -> anyhow::Result<Interrupted> {
    let backup = parent_dir(input).join(backup_file_name(&file_name_lossy(input), job_id));
    let Ok(backup_meta) = fs::symlink_metadata(&backup) else {
        return Ok(Interrupted::NotPlaced);
    };
    if !backup_meta.is_file() {
        return Ok(Interrupted::NotPlaced);
    }
    let same_name = input == final_path || is_same_file(input, final_path);
    let placed = if same_name {
        exists(input)?
    } else {
        !exists(input)? && exists(final_path)?
    };
    if !placed {
        return Ok(Interrupted::NotPlaced);
    }
    let size = fs::metadata(final_path)
        .with_context(|| format!("Could not read {}", final_path.display()))?
        .len();
    Ok(Interrupted::Placed {
        size,
        original_size: backup_meta.len(),
    })
}

/// Delete job `job_id`'s backup of `input` (see [`backup_file_name`]),
/// once [`resume_replace`] found the new file in place and that was
/// recorded. A backup that is gone already is fine.
///
/// Gives up after `timeout` when the folder doesn't answer: the error is
/// then a [`NotAnswering`] (and the deletion may still happen by itself).
pub async fn remove_backup(input: &Path, job_id: Uuid, timeout: Duration) -> anyhow::Result<()> {
    let key = parent_dir(input).join(job_id.to_string());
    let backup = parent_dir(input).join(backup_file_name(&file_name_lossy(input), job_id));
    let removed =
        crate::slow_fs::guarded(
            "remove_backup",
            &key,
            timeout,
            move || match fs::remove_file(&backup) {
                Ok(()) => {
                    sync_dir(&parent_dir(&backup));
                    Ok(())
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!(
                    "Could not delete the backup {}: {e}",
                    backup.display()
                )),
            },
        )
        .await
        .map_err(|e| e.at(parent_dir(&key).as_path()))?;
    removed.map_err(|e| anyhow::anyhow!(e))
}

/// Tests stand in for a share that stops answering while a new file is put
/// in place: [`finalize`] of a job can be held at a chosen step (the
/// system call before it then looks like one that waited for the share).
#[cfg(any(test, feature = "test-hooks"))]
pub mod hold {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, LazyLock, Mutex, PoisonError};
    use std::time::Duration;

    use uuid::Uuid;

    /// Where [`super::finalize`] can be held.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Step {
        /// Before anything (its first look at the files).
        Start,
        /// Right after the original was moved aside as its backup.
        MovedAside,
        /// Right after the new file took its final name.
        Committed,
    }

    type Holds = Vec<(Uuid, Step, Arc<AtomicBool>)>;
    static HOLDS: LazyLock<Mutex<Holds>> = LazyLock::new(Mutex::default);

    /// Hold job `job_id`'s finalize at `step` until the guard is dropped.
    pub fn hold(job_id: Uuid, step: Step) -> Held {
        let reached = Arc::new(AtomicBool::new(false));
        HOLDS.lock().unwrap_or_else(PoisonError::into_inner).push((
            job_id,
            step,
            Arc::clone(&reached),
        ));
        Held {
            job_id,
            step,
            reached,
        }
    }

    /// Releases the hold when dropped.
    #[derive(Debug)]
    pub struct Held {
        job_id: Uuid,
        step: Step,
        reached: Arc<AtomicBool>,
    }

    impl Held {
        /// Whether the finalize got to the held step.
        pub fn reached(&self) -> bool {
            self.reached.load(Ordering::SeqCst)
        }
    }

    impl Drop for Held {
        fn drop(&mut self) {
            HOLDS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retain(|(id, step, _)| !(*id == self.job_id && *step == self.step));
        }
    }

    pub(crate) fn pause(job_id: Uuid, step: Step) {
        loop {
            let held = HOLDS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .find(|(id, s, _)| *id == job_id && *s == step)
                .map(|(_, _, reached)| Arc::clone(reached));
            let Some(reached) = held else {
                return;
            };
            reached.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(not(any(test, feature = "test-hooks")))]
mod hold {
    use uuid::Uuid;

    pub(crate) enum Step {
        Start,
        MovedAside,
        Committed,
    }

    #[inline]
    pub(crate) fn pause(_: Uuid, _: Step) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A replaced file that couldn't keep its owner or group says so.
    #[test]
    fn ownership_changes_get_a_note() {
        assert_eq!(ownership_note((99, 100), (99, 100)), None);
        assert_eq!(
            ownership_note((0, 100), (65534, 100)).as_deref(),
            Some(
                "The new file couldn't keep the original's owner (user 0), so it belongs to user \
                 65534. If your media server can't open it, run Chrysopoeia as the owner of your \
                 media (PUID and PGID)"
            )
        );
        assert!(ownership_note((99, 1001), (99, 100)).unwrap().starts_with(
            "The new file couldn't keep the original's group (group 1001), so it \
                     is in group 100."
        ));
        assert!(ownership_note((0, 0), (65534, 65534)).unwrap().contains(
            "owner and group (user 0, group 0), so it belongs to user 65534 and \
                     group 65534."
        ));
    }

    /// A name that grows past 255 bytes (a long `.ts` name becoming `.mkv`)
    /// and a name that can't be checked are refused before any encoding,
    /// not after it.
    #[tokio::test]
    async fn destinations_that_cant_be_used_are_found_up_front() {
        let wait = Duration::from_secs(30);
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join(format!("{}.ts", "b".repeat(252)));
        fs::write(&input, "x").unwrap();
        let long = input.with_extension("mkv");
        let conflict = destination_conflict(&input, &long, OutputMode::Replace, wait)
            .await
            .unwrap()
            .unwrap();
        assert!(
            conflict.starts_with(
                "The converted file's name would be too long for the disk \
                 (256 bytes"
            ),
            "{conflict}"
        );
        let fits = dir.path().join(format!("{}.mkv", "b".repeat(251)));
        assert_eq!(
            destination_conflict(&input, &fits, OutputMode::Replace, wait).await,
            Ok(None)
        );

        // "Inside" a file: the check fails, which is not a free name.
        let under_a_file = input.join("x.mkv");
        let conflict = destination_conflict(&input, &under_a_file, OutputMode::Folder, wait)
            .await
            .unwrap()
            .unwrap();
        assert!(
            conflict.starts_with("Chrysopoeia couldn't check"),
            "{conflict}"
        );

        // A folder that doesn't answer is not a free name either.
        let _hung = crate::slow_fs::hang::hang(dir.path());
        assert_eq!(
            destination_conflict(
                &input,
                &fits,
                OutputMode::Replace,
                Duration::from_millis(200)
            )
            .await,
            Err(NotAnswering::at(&fits))
        );
    }

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

    const OLD: &[u8] = b"original contents";
    const NEW: &[u8] = b"new, verified contents";
    const NEWER: &[u8] = b"a newer release that replaced the original";

    /// An original in `library/`, a temp file in `scratch/` (another folder,
    /// as with a separate temp folder) and a replace-in-place request.
    fn setup(dir: &Path, final_name: &str) -> (OwnedRequest, fs::Metadata) {
        let input = dir.join("library/Movie.mkv");
        fs::create_dir_all(input.parent().unwrap()).unwrap();
        fs::write(&input, OLD).unwrap();
        let temp = dir
            .join("scratch")
            .join(temp_file_name("Movie", id(), "mkv"));
        fs::create_dir_all(temp.parent().unwrap()).unwrap();
        fs::write(&temp, NEW).unwrap();
        let meta = fs::metadata(&input).unwrap();
        let req = OwnedRequest {
            final_path: dir.join("library").join(final_name),
            input,
            temp,
            mode: OutputMode::Replace,
            job_id: id(),
            keep_dates: true,
            original: Some(FileIdentity::of(&meta)),
            force_copy: true,
            stop: Arc::new(AtomicBool::new(false)),
        };
        (req, meta)
    }

    fn mover<'a>(meta: &'a fs::Metadata, hooks: TestHooks<'a>) -> Mover<'a> {
        Mover {
            job_id: id(),
            force_copy: true,
            original: meta,
            keep_dates: true,
            hooks,
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Replace a file the way Sonarr/Radarr do: a new file renamed over it.
    fn swap_in_newer(path: &Path) {
        let newer = path.with_file_name("incoming.tmp");
        fs::write(&newer, NEWER).unwrap();
        fs::rename(&newer, path).unwrap();
    }

    #[test]
    fn cross_device_copy_finishes_before_the_original_is_touched() {
        let dir = tempfile::tempdir().unwrap();
        let (req, meta) = setup(dir.path(), "Movie.mkv");
        let seen = std::cell::Cell::new(false);
        let check = |staged: &Path| {
            // The copy is complete and beside the original, which is untouched.
            assert_eq!(fs::read(staged).unwrap(), NEW);
            assert_eq!(parent_dir(staged), dir.path().join("library"));
            assert_eq!(fs::read(&req.input).unwrap(), OLD);
            seen.set(true);
        };
        let hooks = TestHooks {
            after_stage: Some(&check),
            fail_commit: false,
            before_commit: None,
        };
        replace_in_place(&req, &mover(&meta, hooks)).unwrap();
        assert!(seen.get());
        assert_eq!(fs::read(&req.input).unwrap(), NEW);
        assert_eq!(names(&dir.path().join("library")), ["Movie.mkv"]);
        assert!(!req.temp.exists(), "the temp file is removed after copying");
    }

    /// Asked to stop (Cancel, or a share that stopped answering) before the
    /// new file took its place: everything done is undone, the original is
    /// where it was, and only the temp file (the caller's) is left.
    #[test]
    fn a_stopped_finalize_undoes_what_it_did() {
        // Before anything.
        let dir = tempfile::tempdir().unwrap();
        let (req, _) = setup(dir.path(), "Movie.mkv");
        req.stop.store(true, Ordering::SeqCst);
        let err = finalize_blocking(&req).unwrap_err();
        assert!(err.is::<Undone>(), "{err:#}");
        assert_eq!(fs::read(&req.input).unwrap(), OLD);
        assert_eq!(names(&dir.path().join("library")), ["Movie.mkv"]);
        assert!(req.temp.exists());

        // After the (long) copy beside the original: the copy goes again.
        for final_name in ["Movie.mkv", "Movie.mp4"] {
            let dir = tempfile::tempdir().unwrap();
            let (req, meta) = setup(dir.path(), final_name);
            let stop = |_: &Path| req.stop.store(true, Ordering::SeqCst);
            let hooks = TestHooks {
                after_stage: Some(&stop),
                fail_commit: false,
                before_commit: None,
            };
            let mut notes = Vec::new();
            let err = if final_name == "Movie.mkv" {
                replace_in_place(&req, &mover(&meta, hooks))
            } else {
                replace_with_new_name(&req, &mover(&meta, hooks), &mut notes)
            }
            .unwrap_err();
            assert!(err.is::<Undone>(), "{final_name}: {err:#}");
            assert_eq!(fs::read(&req.input).unwrap(), OLD);
            assert_eq!(names(&dir.path().join("library")), ["Movie.mkv"]);
            assert!(req.temp.exists());
        }
    }

    #[test]
    fn a_failed_final_rename_restores_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let (req, meta) = setup(dir.path(), "Movie.mkv");
        let hooks = TestHooks {
            after_stage: None,
            fail_commit: true,
            before_commit: None,
        };
        let err = replace_in_place(&req, &mover(&meta, hooks)).unwrap_err();
        let placing = err.downcast_ref::<PlaceError>().expect("a PlaceError");
        assert_eq!(placing.problem, ProblemKind::Destination);
        assert!(
            placing
                .message
                .starts_with("The new file couldn't be put in the original's folder ")
                && placing
                    .message
                    .ends_with(", so the original was kept. Check that folder, then try again."),
            "{err:#}"
        );
        assert!(!placing.message.contains("os error"), "{err:#}");
        assert_eq!(fs::read(&req.input).unwrap(), OLD);
        // No backup and no staged copy left behind; the temp is the caller's.
        assert_eq!(names(&dir.path().join("library")), ["Movie.mkv"]);
        assert!(req.temp.exists());
    }

    #[test]
    fn an_original_replaced_during_the_copy_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (req, meta) = setup(dir.path(), "Movie.mkv");
        let swap = |_: &Path| swap_in_newer(&req.input);
        let hooks = TestHooks {
            after_stage: Some(&swap),
            fail_commit: false,
            before_commit: None,
        };
        let err = replace_in_place(&req, &mover(&meta, hooks)).unwrap_err();
        assert!(err.is::<OriginalChanged>(), "{err:#}");
        assert_eq!(fs::read(&req.input).unwrap(), NEWER);
        assert_eq!(names(&dir.path().join("library")), ["Movie.mkv"]);
    }

    #[test]
    fn an_original_replaced_before_finalize_is_left_alone() {
        for final_name in ["Movie.mkv", "Movie.mp4"] {
            let dir = tempfile::tempdir().unwrap();
            let (req, _) = setup(dir.path(), final_name);
            swap_in_newer(&req.input);
            let err = finalize_blocking(&req).unwrap_err();
            assert!(err.is::<OriginalChanged>(), "{err:#}");
            assert_eq!(
                err.to_string(),
                "The original changed while it was being converted, so it was left alone"
            );
            assert_eq!(fs::read(&req.input).unwrap(), NEWER);
            assert_eq!(names(&dir.path().join("library")), ["Movie.mkv"]);
        }
    }

    #[test]
    fn an_original_changed_after_an_extension_change_keeps_the_newer_file() {
        let dir = tempfile::tempdir().unwrap();
        let (req, meta) = setup(dir.path(), "Movie.mp4");
        let swap = |_: &Path| swap_in_newer(&req.input);
        let hooks = TestHooks {
            after_stage: Some(&swap),
            fail_commit: false,
            before_commit: None,
        };
        let mut notes = Vec::new();
        let err = replace_with_new_name(&req, &mover(&meta, hooks), &mut notes).unwrap_err();
        assert!(err.is::<OriginalChanged>(), "{err:#}");
        assert_eq!(fs::read(&req.input).unwrap(), NEWER);
        assert_eq!(names(&dir.path().join("library")), ["Movie.mkv"]);
    }

    #[test]
    fn identity_ignores_everything_but_size_and_modification_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.mkv");
        fs::write(&path, OLD).unwrap();
        let before = FileIdentity::of(&fs::metadata(&path).unwrap());
        assert_eq!(before.len(), OLD.len() as u64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(FileIdentity::of(&fs::metadata(&path).unwrap()), before);
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(1, 0)).unwrap();
        assert_ne!(FileIdentity::of(&fs::metadata(&path).unwrap()), before);
    }

    #[test]
    fn a_new_extension_moves_the_original_aside_before_the_new_file_goes_in() {
        let dir = tempfile::tempdir().unwrap();
        let (req, meta) = setup(dir.path(), "Movie.mp4");
        let library = dir.path().join("library");
        let backup = library.join(backup_file_name("Movie.mkv", id()));
        let seen = std::cell::Cell::new(false);
        let check = |dst: &Path| {
            // A crash from here on leaves a backup, which recovery uses.
            assert_eq!(dst, req.final_path);
            assert!(!req.input.exists(), "the original is moved aside first");
            assert_eq!(fs::read(&backup).unwrap(), OLD);
            seen.set(true);
        };
        let hooks = TestHooks {
            before_commit: Some(&check),
            ..TestHooks::default()
        };
        let mut notes = Vec::new();
        replace_with_new_name(&req, &mover(&meta, hooks), &mut notes).unwrap();
        assert!(seen.get());
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(names(&library), ["Movie.mp4"]);
        assert_eq!(fs::read(&req.final_path).unwrap(), NEW);
    }

    #[test]
    fn a_failed_rename_to_a_new_extension_restores_the_original() {
        let dir = tempfile::tempdir().unwrap();
        let (req, meta) = setup(dir.path(), "Movie.mp4");
        let hooks = TestHooks {
            fail_commit: true,
            ..TestHooks::default()
        };
        let mut notes = Vec::new();
        let err = replace_with_new_name(&req, &mover(&meta, hooks), &mut notes).unwrap_err();
        let placing = err.downcast_ref::<PlaceError>().expect("a PlaceError");
        assert_eq!(placing.problem, ProblemKind::Destination);
        assert!(
            placing
                .message
                .ends_with(", so the original was kept. Check that folder, then try again."),
            "{err:#}"
        );
        assert_eq!(names(&dir.path().join("library")), ["Movie.mkv"]);
        assert_eq!(fs::read(&req.input).unwrap(), OLD);
    }

    #[test]
    fn a_file_taking_the_new_name_at_the_last_moment_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let (req, meta) = setup(dir.path(), "Movie.mp4");
        let intruder = |_: &Path| fs::write(&req.final_path, NEWER).unwrap();
        let hooks = TestHooks {
            after_stage: Some(&intruder),
            ..TestHooks::default()
        };
        let mut notes = Vec::new();
        let err = replace_with_new_name(&req, &mover(&meta, hooks), &mut notes).unwrap_err();
        assert_eq!(
            err.to_string(),
            "A file named \"Movie.mp4\" appeared next to the original while it was being \
             converted, so the new file wasn't put in place. Move or rename that file, then \
             convert this one again."
        );
        assert_eq!(
            err.downcast_ref::<PlaceError>().map(|e| e.problem),
            Some(ProblemKind::Destination)
        );
        assert_eq!(fs::read(&req.input).unwrap(), OLD);
        assert_eq!(fs::read(&req.final_path).unwrap(), NEWER);
        assert_eq!(
            names(&dir.path().join("library")),
            ["Movie.mkv", "Movie.mp4"]
        );
    }

    /// The same relative path in two libraries saved to one output folder
    /// (`/media/movies/Movies/Frozen.mkv` and `/media/kids/Movies/Frozen.mkv`
    /// both become `/out/Movies/Frozen.mkv`): when both jobs put their file
    /// in place at the same moment, the second is refused and the first's
    /// file is kept as it is. (Without the claim on the name, both found it
    /// free and the second rename replaced the first's file.)
    #[test]
    fn two_jobs_putting_a_file_under_one_name_never_overwrite_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out/Movies/Frozen.mkv");
        let request = |library: &str, job_id: Uuid, contents: &[u8]| {
            let input = dir.path().join(library).join("Movies/Frozen.mkv");
            fs::create_dir_all(input.parent().unwrap()).unwrap();
            fs::write(&input, OLD).unwrap();
            let temp = dir
                .path()
                .join("scratch")
                .join(temp_file_name("Frozen", job_id, "mkv"));
            fs::create_dir_all(temp.parent().unwrap()).unwrap();
            fs::write(&temp, contents).unwrap();
            let meta = fs::metadata(&input).unwrap();
            let req = OwnedRequest {
                final_path: out.clone(),
                input,
                temp,
                mode: OutputMode::Folder,
                job_id,
                keep_dates: false,
                original: Some(FileIdentity::of(&meta)),
                force_copy: true,
                stop: Arc::new(AtomicBool::new(false)),
            };
            (req, meta)
        };
        let (first, first_meta) = request("movies", Uuid::new_v4(), NEW);
        let (second, second_meta) = request("kids", Uuid::new_v4(), NEWER);
        let second_result = std::cell::RefCell::new(None);
        // The second job finishes while the first is about to rename its
        // staged file into place.
        let race = |_: &Path| {
            let mover = Mover {
                job_id: second.job_id,
                force_copy: true,
                original: &second_meta,
                keep_dates: false,
                hooks: TestHooks::default(),
            };
            *second_result.borrow_mut() = Some(place_in_folder(&second, &mover));
        };
        let mover = Mover {
            job_id: first.job_id,
            force_copy: true,
            original: &first_meta,
            keep_dates: false,
            hooks: TestHooks {
                before_commit: Some(&race),
                ..TestHooks::default()
            },
        };
        place_in_folder(&first, &mover).unwrap();
        let err = second_result.into_inner().unwrap().unwrap_err();
        assert_eq!(
            err.to_string(),
            "A file named \"Frozen.mkv\" appeared in the output folder while this file was \
             being converted, so it wasn't overwritten. Move or delete it, then convert this \
             file again."
        );
        assert!(output_name_taken(&err.to_string()));
        assert_eq!(
            err.downcast_ref::<PlaceError>().map(|e| e.problem),
            Some(ProblemKind::Destination)
        );
        assert_eq!(fs::read(&out).unwrap(), NEW);
        // Nothing of the second job's was left there; both originals stay.
        assert_eq!(names(out.parent().unwrap()), ["Frozen.mkv"]);
        assert_eq!(fs::read(&first.input).unwrap(), OLD);
        assert_eq!(fs::read(&second.input).unwrap(), OLD);
        // The name is free again for later jobs.
        assert!(NameClaim::take(&out).is_some());
    }

    /// The errors that say the name in the output folder was taken are
    /// recognised (the server then names the library whose file has it).
    #[tokio::test]
    async fn a_taken_name_in_the_output_folder_is_recognised() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("kids/Movies/Frozen.mkv");
        let out = dir.path().join("out/Movies/Frozen.mkv");
        fs::create_dir_all(input.parent().unwrap()).unwrap();
        fs::create_dir_all(out.parent().unwrap()).unwrap();
        fs::write(&input, OLD).unwrap();
        fs::write(&out, NEW).unwrap();
        let conflict =
            destination_conflict(&input, &out, OutputMode::Folder, Duration::from_secs(5))
                .await
                .unwrap()
                .unwrap();
        assert!(output_name_taken(&conflict), "{conflict}");
        assert!(output_name_taken(
            &appeared_in_output_folder(&out).to_string()
        ));
        // Next to the original, or anything else: not about the output folder.
        let beside = dir.path().join("kids/Movies/Frozen.mp4");
        fs::write(&beside, NEW).unwrap();
        let replace =
            destination_conflict(&input, &beside, OutputMode::Replace, Duration::from_secs(5))
                .await
                .unwrap()
                .unwrap();
        assert!(!output_name_taken(&replace), "{replace}");
        assert!(!output_name_taken(
            "Chrysopoeia doesn't have permission to write in the output folder /out"
        ));
    }

    /// The files a crash can leave, and what [`resume_blocking`] makes of
    /// them.
    #[test]
    fn resume_finishes_only_replacements_whose_new_file_was_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path();
        let input = lib.join("Movie.mp4");
        let new_name = lib.join("Movie.mkv");
        let backup = lib.join(backup_file_name("Movie.mp4", id()));

        // New extension, crash after the new file went in: complete. Only
        // looked at: the backup stays until it is removed.
        fs::write(&backup, OLD).unwrap();
        fs::write(&new_name, NEW).unwrap();
        for _ in 0..2 {
            assert_eq!(
                resume_blocking(&input, &new_name, id()).unwrap(),
                Interrupted::Placed {
                    size: NEW.len() as u64,
                    original_size: OLD.len() as u64
                }
            );
        }
        assert!(backup.exists());
        fs::remove_file(&backup).unwrap();
        assert_eq!(names(lib), ["Movie.mkv"]);

        // New extension, crash before the new file went in: not placed, the
        // backup stays for recover_artifact to put back.
        fs::remove_file(&new_name).unwrap();
        fs::write(&backup, OLD).unwrap();
        assert_eq!(
            resume_blocking(&input, &new_name, id()).unwrap(),
            Interrupted::NotPlaced
        );
        assert!(backup.exists());
        fs::remove_file(&backup).unwrap();

        // No backup: a file under the new name next to the original is not
        // ours to claim.
        fs::write(&input, OLD).unwrap();
        fs::write(&new_name, NEWER).unwrap();
        assert_eq!(
            resume_blocking(&input, &new_name, id()).unwrap(),
            Interrupted::NotPlaced
        );
        assert_eq!(names(lib), ["Movie.mkv", "Movie.mp4"]);

        // Same name, crash after the swap: complete.
        fs::remove_file(&new_name).unwrap();
        fs::write(&input, NEW).unwrap();
        fs::write(&backup, OLD).unwrap();
        assert_eq!(
            resume_blocking(&input, &input, id()).unwrap(),
            Interrupted::Placed {
                size: NEW.len() as u64,
                original_size: OLD.len() as u64
            }
        );
        let backup_name = backup.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(names(lib), [backup_name.as_str(), "Movie.mp4"]);
        fs::remove_file(&backup).unwrap();

        // Same name, crash with the original moved aside: not placed.
        fs::rename(&input, &backup).unwrap();
        assert_eq!(
            resume_blocking(&input, &input, id()).unwrap(),
            Interrupted::NotPlaced
        );
        assert!(backup.exists());
    }
}
