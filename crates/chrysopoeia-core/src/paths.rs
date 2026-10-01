//! Naming of the temporary and backup files Chrysopoeia writes.
//!
//! Every such file contains [`ARTIFACT_MARKER`] so the scanner can ignore it
//! and the server can clean up after a crash.

use uuid::Uuid;

/// Present in the name of every temporary or backup file we create.
pub const ARTIFACT_MARKER: &str = ".chrysopoeia-";

fn short_id(job_id: Uuid) -> String {
    job_id.simple().to_string()[..8].to_string()
}

/// Hidden in-progress output: `.{stem}.chrysopoeia-{id8}.tmp.{ext}`.
pub fn temp_file_name(stem: &str, job_id: Uuid, ext: &str) -> String {
    format!(".{stem}{ARTIFACT_MARKER}{}.tmp.{ext}", short_id(job_id))
}

/// Hidden backup of an original while it is being replaced:
/// `.{file_name}.chrysopoeia-{id8}.bak`.
pub fn backup_file_name(file_name: &str, job_id: Uuid) -> String {
    format!(".{file_name}{ARTIFACT_MARKER}{}.bak", short_id(job_id))
}

/// Whether a file name is one of our temporary or backup files.
pub fn is_artifact(file_name: &str) -> bool {
    file_name.contains(ARTIFACT_MARKER)
}

/// Whether a temporary or backup file name was made by the job `job_id`
/// (its name carries the job's short id).
pub fn is_artifact_of(file_name: &str, job_id: Uuid) -> bool {
    artifact_job(file_name) == Some(short_id(job_id).as_str())
}

/// The short job id (8 hex digits) a temporary or backup file name carries.
pub fn artifact_job(file_name: &str) -> Option<&str> {
    let at = file_name.rfind(ARTIFACT_MARKER)?;
    let id = file_name.get(at + ARTIFACT_MARKER.len()..)?.get(..8)?;
    id.chars().all(|c| c.is_ascii_hexdigit()).then_some(id)
}

/// Whether a file name is one of our backups.
pub fn is_backup(file_name: &str) -> bool {
    is_artifact(file_name) && file_name.ends_with(".bak")
}

/// For a backup name produced by [`backup_file_name`], the original file name.
pub fn original_name_from_backup(backup_name: &str) -> Option<String> {
    let rest = backup_name.strip_prefix('.')?.strip_suffix(".bak")?;
    let marker_at = rest.rfind(ARTIFACT_MARKER)?;
    let (name, _) = rest.split_at(marker_at);
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_roundtrip() {
        let id = Uuid::parse_str("0123456789abcdef0123456789abcdef").unwrap();
        let tmp = temp_file_name("Movie (2020)", id, "mkv");
        assert_eq!(tmp, ".Movie (2020).chrysopoeia-01234567.tmp.mkv");
        assert!(is_artifact(&tmp));
        assert!(!is_backup(&tmp));

        let bak = backup_file_name("Movie (2020).mkv", id);
        assert_eq!(bak, ".Movie (2020).mkv.chrysopoeia-01234567.bak");
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
            temp_file_name("Show.chrysopoeia-edition", id, "mkv"),
            backup_file_name("Movie.mkv", id),
        ] {
            assert_eq!(artifact_job(&name), Some("01234567"), "{name}");
            assert!(is_artifact_of(&name, id));
            assert!(!is_artifact_of(&name, other));
        }
        assert_eq!(artifact_job("Movie.mkv"), None);
        assert_eq!(artifact_job(".x.chrysopoeia-zz.tmp.mkv"), None);
    }
}
