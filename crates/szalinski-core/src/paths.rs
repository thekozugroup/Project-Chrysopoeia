//! Naming of the temporary and backup files Szalinski writes.
//!
//! Every such file contains [`ARTIFACT_MARKER`] so the scanner can ignore it
//! and the server can clean up after a crash. Files made before the rename
//! from Chrysopoeia carry [`LEGACY_ARTIFACT_MARKER`] instead; they are
//! recognised the same way, so an upgrade never loses track of a backup.

use uuid::Uuid;

/// Present in the name of every temporary or backup file we create.
pub const ARTIFACT_MARKER: &str = ".szalinski-";

/// The marker of files made before the rename (Chrysopoeia 0.2).
pub const LEGACY_ARTIFACT_MARKER: &str = ".chrysopoeia-";

/// Where the last marker (current or legacy) starts in `name`, and its length.
fn marker_at(name: &str) -> Option<(usize, usize)> {
    let current = name
        .rfind(ARTIFACT_MARKER)
        .map(|at| (at, ARTIFACT_MARKER.len()));
    let legacy = name
        .rfind(LEGACY_ARTIFACT_MARKER)
        .map(|at| (at, LEGACY_ARTIFACT_MARKER.len()));
    match (current, legacy) {
        (Some(c), Some(l)) => Some(if c.0 >= l.0 { c } else { l }),
        (c, l) => c.or(l),
    }
}

fn short_id(job_id: Uuid) -> String {
    job_id.simple().to_string()[..8].to_string()
}

/// Hidden in-progress output: `.{stem}.szalinski-{id8}.tmp.{ext}`.
pub fn temp_file_name(stem: &str, job_id: Uuid, ext: &str) -> String {
    format!(".{stem}{ARTIFACT_MARKER}{}.tmp.{ext}", short_id(job_id))
}

/// Hidden backup of an original while it is being replaced:
/// `.{file_name}.szalinski-{id8}.bak`.
pub fn backup_file_name(file_name: &str, job_id: Uuid) -> String {
    format!(".{file_name}{ARTIFACT_MARKER}{}.bak", short_id(job_id))
}

/// The name [`backup_file_name`] gave the same backup before the rename.
pub fn legacy_backup_file_name(file_name: &str, job_id: Uuid) -> String {
    format!(
        ".{file_name}{LEGACY_ARTIFACT_MARKER}{}.bak",
        short_id(job_id)
    )
}

/// Every name job `job_id`'s backup of `file_name` may have: the current
/// one first, then the one used before the rename.
pub fn backup_file_names(file_name: &str, job_id: Uuid) -> [String; 2] {
    [
        backup_file_name(file_name, job_id),
        legacy_backup_file_name(file_name, job_id),
    ]
}

/// Whether a file name is one of our temporary or backup files.
pub fn is_artifact(file_name: &str) -> bool {
    file_name.contains(ARTIFACT_MARKER) || file_name.contains(LEGACY_ARTIFACT_MARKER)
}

/// Whether a temporary or backup file name was made by the job `job_id`
/// (its name carries the job's short id).
pub fn is_artifact_of(file_name: &str, job_id: Uuid) -> bool {
    artifact_job(file_name) == Some(short_id(job_id).as_str())
}

/// The short job id (8 hex digits) a temporary or backup file name carries.
pub fn artifact_job(file_name: &str) -> Option<&str> {
    let (at, len) = marker_at(file_name)?;
    let id = file_name.get(at + len..)?.get(..8)?;
    id.chars().all(|c| c.is_ascii_hexdigit()).then_some(id)
}

/// Whether a file name is one of our backups.
pub fn is_backup(file_name: &str) -> bool {
    is_artifact(file_name) && file_name.ends_with(".bak")
}

/// For a backup name produced by [`backup_file_name`], the original file name.
pub fn original_name_from_backup(backup_name: &str) -> Option<String> {
    let rest = backup_name.strip_prefix('.')?.strip_suffix(".bak")?;
    let (at, _) = marker_at(rest)?;
    let (name, _) = rest.split_at(at);
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_roundtrip() {
        let id = Uuid::parse_str("0123456789abcdef0123456789abcdef").unwrap();
        let tmp = temp_file_name("Movie (2020)", id, "mkv");
        assert_eq!(tmp, ".Movie (2020).szalinski-01234567.tmp.mkv");
        assert!(is_artifact(&tmp));
        assert!(!is_backup(&tmp));

        let bak = backup_file_name("Movie (2020).mkv", id);
        assert_eq!(bak, ".Movie (2020).mkv.szalinski-01234567.bak");
        assert!(is_backup(&bak));
        assert_eq!(
            original_name_from_backup(&bak).as_deref(),
            Some("Movie (2020).mkv")
        );
        assert!(!is_artifact("Movie (2020).mkv"));
    }

    #[test]
    fn artifacts_name_their_job() {
        let id = Uuid::parse_str("0123456789abcdef0123456789abcdef").unwrap();
        let other = Uuid::parse_str("fedcba9876543210fedcba9876543210").unwrap();
        for name in [
            temp_file_name("Show.szalinski-edition", id, "mkv"),
            backup_file_name("Movie.mkv", id),
        ] {
            assert_eq!(artifact_job(&name), Some("01234567"), "{name}");
            assert!(is_artifact_of(&name, id));
            assert!(!is_artifact_of(&name, other));
        }
        assert_eq!(artifact_job("Movie.mkv"), None);
        let legacy = legacy_backup_file_name("Movie.mkv", id);
        assert_eq!(legacy, ".Movie.mkv.chrysopoeia-01234567.bak");
        assert!(is_artifact(&legacy) && is_backup(&legacy) && is_artifact_of(&legacy, id));
        assert_eq!(
            original_name_from_backup(&legacy).as_deref(),
            Some("Movie.mkv")
        );
        assert_eq!(
            backup_file_names("Movie.mkv", id),
            [backup_file_name("Movie.mkv", id), legacy]
        );
        assert!(is_artifact(".Movie.chrysopoeia-01234567.tmp.mkv"));
        assert_eq!(artifact_job(".x.szalinski-zz.tmp.mkv"), None);
    }
}
