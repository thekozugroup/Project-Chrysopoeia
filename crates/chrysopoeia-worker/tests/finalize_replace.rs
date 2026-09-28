//! Crash-safe placement of finished files and recovery of leftovers.

mod support;

use std::path::{Path, PathBuf};

use chrysopoeia_core::paths::{backup_file_name, temp_file_name};
use chrysopoeia_core::{OutputMode, ProblemKind};
use chrysopoeia_worker::finalize::{
    FileIdentity, FinalizeRequest, OriginalChanged, Recovery, finalize, recover_artifact,
};
use filetime::FileTime;
use uuid::Uuid;

/// The kind of problem a failed `finalize` reports.
fn problem_of(err: &anyhow::Error) -> ProblemKind {
    err.downcast_ref::<chrysopoeia_worker::finalize::PlaceError>()
        .map(|e| e.problem)
        .expect("finalize explains its failures")
}

const OLD: &[u8] = b"original contents";
const NEW: &[u8] = b"new, verified contents that are longer";

fn job_id() -> Uuid {
    Uuid::parse_str("0123456789abcdef0123456789abcdef").unwrap()
}

fn old_time() -> FileTime {
    // 2001-09-09T01:46:40Z
    FileTime::from_unix_time(1_000_000_000, 0)
}

/// An "original" with known contents, mode and dates.
fn make_original(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, OLD).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o640)).unwrap();
    }
    filetime::set_file_times(path, old_time(), old_time()).unwrap();
}

fn make_temp(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let temp = dir.join(temp_file_name(stem, job_id(), ext));
    std::fs::write(&temp, NEW).unwrap();
    temp
}

fn request<'a>(
    input: &'a Path,
    temp: &'a Path,
    final_path: &'a Path,
    mode: OutputMode,
) -> FinalizeRequest<'a> {
    FinalizeRequest {
        input,
        temp,
        final_path,
        mode,
        job_id: job_id(),
        keep_dates: true,
        // As a job does: the original as it was when the encode started.
        original: std::fs::metadata(input).ok().map(|m| FileIdentity::of(&m)),
        force_copy: false,
    }
}

fn mtime(path: &Path) -> FileTime {
    FileTime::from_last_modification_time(&std::fs::metadata(path).unwrap())
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

#[tokio::test]
async fn same_path_replace_swaps_in_the_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("Movie (2020).mkv");
    make_original(&input);
    let temp = make_temp(dir.path(), "Movie (2020)", "mkv");

    let placed = finalize(&request(&input, &temp, &input, OutputMode::Replace))
        .await
        .unwrap();
    assert_eq!(placed.size, NEW.len() as u64);
    assert!(placed.notes.is_empty());
    assert_eq!(std::fs::read(&input).unwrap(), NEW);
    assert!(!temp.exists());
    assert!(
        support::artifacts_in(dir.path()).is_empty(),
        "no backup left"
    );
    assert_eq!(mtime(&input), old_time());
    #[cfg(unix)]
    assert_eq!(mode(&input), 0o640);
}

#[tokio::test]
async fn new_extension_moves_in_and_deletes_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("Old Home Video.avi");
    make_original(&input);
    let temp = make_temp(dir.path(), "Old Home Video", "mkv");
    let final_path = dir.path().join("Old Home Video.mkv");

    finalize(&request(&input, &temp, &final_path, OutputMode::Replace))
        .await
        .unwrap();
    assert!(!input.exists());
    assert_eq!(std::fs::read(&final_path).unwrap(), NEW);
    assert_eq!(mtime(&final_path), old_time());
    assert_eq!(support::walk(dir.path()), [final_path]);
}

#[tokio::test]
async fn refuses_to_overwrite_an_unrelated_file() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("Film.avi");
    make_original(&input);
    let temp = make_temp(dir.path(), "Film", "mkv");
    let final_path = dir.path().join("Film.mkv");
    std::fs::write(&final_path, b"someone else's file").unwrap();

    let err = finalize(&request(&input, &temp, &final_path, OutputMode::Replace))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "A file named \"Film.mkv\" appeared next to the original while it was being converted, \
         so the new file wasn't put in place. Move or rename that file, then convert this one \
         again."
    );
    assert_eq!(problem_of(&err), ProblemKind::Destination);
    assert_eq!(std::fs::read(&input).unwrap(), OLD);
    assert_eq!(std::fs::read(&final_path).unwrap(), b"someone else's file");
    // The temp file is the caller's to delete.
    assert!(temp.exists());
}

#[tokio::test]
async fn folder_mode_mirrors_and_never_touches_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("media");
    let input = library.join("TV/Show/S01E01.ts");
    make_original(&input);
    let temp = make_temp(&dir.path().join("scratch"), "S01E01", "mkv");
    let final_path = dir.path().join("out/TV/Show/S01E01.mkv");

    finalize(&request(&input, &temp, &final_path, OutputMode::Folder))
        .await
        .unwrap();
    assert_eq!(std::fs::read(&input).unwrap(), OLD);
    assert_eq!(mtime(&input), old_time());
    assert_eq!(std::fs::read(&final_path).unwrap(), NEW);
    assert!(!temp.exists());

    // A second run must not overwrite the first result.
    let temp = make_temp(&dir.path().join("scratch"), "S01E01", "mkv");
    let err = finalize(&request(&input, &temp, &final_path, OutputMode::Folder))
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("A file named \"S01E01.mkv\" appeared in the output folder"),
        "{err:#}"
    );
    assert_eq!(problem_of(&err), ProblemKind::Destination);
    assert_eq!(std::fs::read(&input).unwrap(), OLD);
}

#[tokio::test]
async fn keep_dates_off_leaves_a_fresh_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("a.mkv");
    make_original(&input);
    let temp = make_temp(dir.path(), "a", "mkv");
    let mut req = request(&input, &temp, &input, OutputMode::Replace);
    req.keep_dates = false;
    finalize(&req).await.unwrap();
    assert!(mtime(&input) > old_time());
    #[cfg(unix)]
    assert_eq!(mode(&input), 0o640, "permissions are always preserved");
}

#[tokio::test]
async fn cross_device_copy_path_places_the_file_and_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("media/Movie.mkv");
    make_original(&input);
    let temp = make_temp(&dir.path().join("scratch"), "Movie", "mkv");
    let mut req = request(&input, &temp, &input, OutputMode::Replace);
    req.force_copy = true;

    let placed = finalize(&req).await.unwrap();
    assert_eq!(placed.size, NEW.len() as u64);
    assert_eq!(std::fs::read(&input).unwrap(), NEW);
    assert!(!temp.exists(), "the temp file is removed after copying");
    assert!(support::artifacts_in(dir.path()).is_empty());
    assert_eq!(mtime(&input), old_time());
    #[cfg(unix)]
    assert_eq!(mode(&input), 0o640);

    // Extension change and folder mode take the same copy path.
    let other = dir.path().join("media/Clip.avi");
    make_original(&other);
    let temp = make_temp(&dir.path().join("scratch"), "Clip", "mkv");
    let final_path = dir.path().join("media/Clip.mkv");
    let mut req = request(&other, &temp, &final_path, OutputMode::Replace);
    req.force_copy = true;
    finalize(&req).await.unwrap();
    assert!(!other.exists());
    assert_eq!(std::fs::read(&final_path).unwrap(), NEW);
    assert!(support::artifacts_in(dir.path()).is_empty());
}

#[tokio::test]
async fn missing_or_empty_temp_leaves_the_original_alone() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("a.mkv");
    make_original(&input);
    let temp = dir.path().join(temp_file_name("a", job_id(), "mkv"));
    let err = finalize(&request(&input, &temp, &input, OutputMode::Replace))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "The converted file disappeared before it could be put in place, so the original was \
         kept. Try again."
    );

    std::fs::write(&temp, b"").unwrap();
    let err = finalize(&request(&input, &temp, &input, OutputMode::Replace))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "The converted file turned out empty, so the original was kept. Try again."
    );
    assert_eq!(std::fs::read(&input).unwrap(), OLD);
    assert!(
        support::artifacts_in(dir.path()).len() == 1,
        "only the temp"
    );
}

#[tokio::test]
async fn missing_original_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("gone.mkv");
    let temp = make_temp(dir.path(), "gone", "mkv");
    let err = finalize(&request(&input, &temp, &input, OutputMode::Replace))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "The original is no longer there, so the new file wasn't put in place. It may have been \
         moved or deleted while it was being converted. If it was moved, scan the library again \
         to find it."
    );
    assert_eq!(problem_of(&err), ProblemKind::SourceChanged);
}

#[tokio::test]
async fn recovery_deletes_stale_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let temp = make_temp(dir.path(), "Movie", "mkv");
    assert_eq!(
        recover_artifact(&temp).await.unwrap(),
        Recovery::DeletedTemp
    );
    assert!(!temp.exists());
    // Partial cross-device copies use the temp naming too.
    let partial = make_temp(dir.path(), "Movie", "part.mkv");
    assert_eq!(
        recover_artifact(&partial).await.unwrap(),
        Recovery::DeletedTemp
    );
}

#[tokio::test]
async fn recovery_restores_a_backup_whose_original_is_missing() {
    // A crash between "original → backup" and "new → original".
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("Movie (2020).mkv");
    let backup = dir
        .path()
        .join(backup_file_name("Movie (2020).mkv", job_id()));
    std::fs::write(&backup, OLD).unwrap();

    assert_eq!(
        recover_artifact(&backup).await.unwrap(),
        Recovery::RestoredBackup(original.clone())
    );
    assert_eq!(std::fs::read(&original).unwrap(), OLD);
    assert!(!backup.exists());
}

#[tokio::test]
async fn recovery_deletes_a_backup_once_the_new_file_is_in_place() {
    // A crash after the new file was moved in, before the backup was deleted.
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("Movie.mkv");
    std::fs::write(&original, NEW).unwrap();
    let backup = dir.path().join(backup_file_name("Movie.mkv", job_id()));
    std::fs::write(&backup, OLD).unwrap();

    assert_eq!(
        recover_artifact(&backup).await.unwrap(),
        Recovery::DeletedBackup
    );
    assert_eq!(std::fs::read(&original).unwrap(), NEW);
    assert!(!backup.exists());
}

#[tokio::test]
async fn recovery_leaves_everything_else_alone() {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("Movie.mkv");
    std::fs::write(&media, OLD).unwrap();
    assert_eq!(recover_artifact(&media).await.unwrap(), Recovery::Untouched);
    assert!(media.exists());

    let missing = dir.path().join(temp_file_name("gone", job_id(), "mkv"));
    assert_eq!(
        recover_artifact(&missing).await.unwrap(),
        Recovery::Untouched
    );

    let folder = dir.path().join(temp_file_name("folder", job_id(), "mkv"));
    std::fs::create_dir(&folder).unwrap();
    assert_eq!(
        recover_artifact(&folder).await.unwrap(),
        Recovery::Untouched
    );
    assert!(folder.exists());
}

#[tokio::test]
async fn a_failed_cross_device_copy_never_touches_the_original() {
    // Something already occupies the hidden staging name, so the copy into
    // the library folder fails before the original is moved at all.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("media/Movie.mkv");
    make_original(&input);
    let temp = make_temp(&dir.path().join("scratch"), "Movie", "mkv");
    let staging = dir
        .path()
        .join("media")
        .join(temp_file_name("Movie", job_id(), "part.mkv"));
    std::fs::create_dir(&staging).unwrap();
    let mut req = request(&input, &temp, &input, OutputMode::Replace);
    req.force_copy = true;

    let err = finalize(&req).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        format!(
            "The new file couldn't be put in the original's folder {} because a folder is in \
             the way where a file should be, so the original was kept. Check that folder, then \
             try again.",
            dir.path().join("media").display()
        )
    );
    assert_eq!(problem_of(&err), ProblemKind::Destination);
    assert_eq!(std::fs::read(&input).unwrap(), OLD);
    assert_eq!(mtime(&input), old_time());
    assert!(temp.exists(), "the temp file is the caller's to delete");
    // No backup was made.
    assert_eq!(support::walk(&dir.path().join("media")), [input]);
}

#[tokio::test]
async fn a_replaced_original_is_never_overwritten() {
    // Sonarr/Radarr swapped in a newer release while the job was encoding.
    for (final_name, mode) in [
        ("Movie.mkv", OutputMode::Replace),
        ("Movie.mp4", OutputMode::Replace),
        ("out/Movie.mkv", OutputMode::Folder),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("Movie.mkv");
        make_original(&input);
        let temp = make_temp(&dir.path().join("scratch"), "Movie", "mkv");
        let final_path = dir.path().join(final_name);
        let req = request(&input, &temp, &final_path, mode);

        let newer = dir.path().join("download.part");
        std::fs::write(&newer, b"a newer release").unwrap();
        std::fs::rename(&newer, &input).unwrap();

        let err = finalize(&req).await.unwrap_err();
        assert!(err.is::<OriginalChanged>(), "{final_name}: {err:#}");
        assert_eq!(std::fs::read(&input).unwrap(), b"a newer release");
        assert!(!dir.path().join("Movie.mp4").exists());
        assert!(
            !dir.path().join("out").exists() || support::walk(&dir.path().join("out")).is_empty()
        );
        assert!(
            support::artifacts_in(dir.path()).iter().all(|p| p == &temp),
            "only the caller's temp file remains"
        );
    }
}

#[tokio::test]
async fn an_unchanged_original_with_new_permissions_is_still_replaced() {
    // chmod/chown (e.g. Unraid's "New Permissions") does not count as a change.
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("Movie.mkv");
    make_original(&input);
    let temp = make_temp(dir.path(), "Movie", "mkv");
    let req = request(&input, &temp, &input, OutputMode::Replace);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&input, std::fs::Permissions::from_mode(0o666)).unwrap();
    }
    finalize(&req).await.unwrap();
    assert_eq!(std::fs::read(&input).unwrap(), NEW);
}
