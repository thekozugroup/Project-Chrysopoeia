//! The drives and shares the folders Chrysopoeia uses are mounted from.
//!
//! An unmounted share leaves its mount point behind as an ordinary folder
//! on the disk below: empty, or holding whatever was there before the
//! share was mounted over it. Taken for the share, it would have the files
//! on the share taken for gone (a job whose new file was being put in place
//! when the server stopped would be settled as "not in place", and its
//! backup put back next to the new file once the share is back), and new
//! files written into it, hidden under the share once it is mounted again.
//!
//! So the mount points each library folder, the output folder and the work
//! folder sit on or under are remembered (`folder_mounts`, read from
//! `/proc/self/mountinfo` without touching any share), adding any found
//! mounted whenever the folder is looked at. While one of them isn't
//! mounted, the folder is not connected ([`Mounted::No`]): its library is
//! offline, jobs that use it wait, nothing is written there, and a job
//! whose new file may have been put in place is never settled on it. A
//! folder's mount points are forgotten only when it stops being used for
//! that: a library removed (or added again), the output folder or the work
//! folder changed in Settings.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use chrysopoeia_worker::slow_fs;

use crate::db;
use crate::state::{AppState, lock};

/// What is remembered, by folder (loaded from the database on first use).
#[derive(Debug, Default)]
pub struct ShareMounts {
    known: std::sync::Mutex<HashMap<PathBuf, BTreeSet<PathBuf>>>,
}

/// Whether the mount points a folder is known to sit on are mounted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mounted {
    /// They all are: these, with any found mounted above it now.
    Yes(Vec<PathBuf>),
    /// This one isn't: the folder is an ordinary folder on the disk below.
    No(PathBuf),
    /// Can't tell (the list of mounts or the database couldn't be read).
    Unknown,
}

/// The reason given for a folder on a share that isn't mounted.
pub fn not_connected(mount: &Path) -> String {
    format!(
        "The drive or share mounted at {} isn't connected. Reconnect it, and its conversions \
         continue.",
        mount.display()
    )
}

/// Whether the mount points `folder` is known to sit on (at or above it)
/// are all mounted; when they are, any others mounted at or above it now
/// are remembered too.
pub async fn check(state: &AppState, folder: &Path) -> Mounted {
    // A folder that can't be stored as text is never remembered.
    let Some(key) = folder.to_str() else {
        return Mounted::Yes(Vec::new());
    };
    let cached = lock(&state.mounts.known).get(folder).cloned();
    let known = match cached {
        Some(known) => known,
        None => match db::folder_mounts::get(state.db.pool(), key).await {
            Ok(rows) => {
                let rows: BTreeSet<PathBuf> = rows.into_iter().map(PathBuf::from).collect();
                lock(&state.mounts.known)
                    .entry(folder.to_path_buf())
                    .or_default()
                    .extend(rows.iter().cloned());
                rows
            }
            Err(e) => {
                tracing::debug!(folder = %folder.display(), "could not read the folder's mounts: {e}");
                return Mounted::Unknown;
            }
        },
    };
    let Some(now) = slow_fs::mount_points().await else {
        return if known.is_empty() {
            Mounted::Yes(Vec::new())
        } else {
            Mounted::Unknown
        };
    };
    // The outermost one that is gone: the share itself, when a mount
    // inside it went with it.
    if let Some(gone) = known
        .iter()
        .filter(|m| !now.iter().any(|n| n.path == **m))
        .min_by_key(|m| m.components().count())
    {
        return Mounted::No(gone.clone());
    }
    // Not "/", which is always there, nor a share an automounter mounts on
    // demand (it comes and goes by itself, and a look at its folder mounts
    // it again; it leaves no ordinary folder behind).
    let above: BTreeSet<PathBuf> = now
        .into_iter()
        .filter(|m| {
            !m.on_demand
                && folder.starts_with(&m.path)
                && m.path.parent().is_some()
                && m.path.to_str().is_some()
        })
        .map(|m| m.path)
        .collect();
    let new: Vec<String> = above
        .difference(&known)
        .filter_map(|m| m.to_str().map(str::to_string))
        .collect();
    if !new.is_empty() {
        lock(&state.mounts.known)
            .entry(folder.to_path_buf())
            .or_default()
            .extend(above.iter().cloned());
        let saved = async {
            let mut tx = state.db.write_tx().await?;
            db::folder_mounts::add(&mut tx, key, &new).await?;
            tx.commit().await
        }
        .await;
        match saved {
            Ok(()) => {
                tracing::debug!(folder = %folder.display(), mounts = ?new, "remembered the folder's mounts")
            }
            Err(e) => {
                tracing::warn!(folder = %folder.display(), "could not record the folder's mounts: {e}")
            }
        }
    }
    Mounted::Yes(known.union(&above).cloned().collect())
}

/// Forget what is known about `folder` (it stops being used, or is chosen
/// again): its mounts are learned afresh from the next look at it.
pub async fn forget(state: &AppState, folder: &Path) {
    lock(&state.mounts.known).remove(folder);
    let Some(key) = folder.to_str() else {
        return;
    };
    let forgotten = async {
        let mut tx = state.db.write_tx().await?;
        db::folder_mounts::forget(&mut tx, key).await?;
        tx.commit().await
    }
    .await;
    if let Err(e) = forgotten {
        tracing::warn!(folder = %folder.display(), "could not forget the folder's mounts: {e}");
    }
}

/// [`forget`] `folder`, unless it is still used: a library folder, the
/// output folder or the work folder.
pub async fn forget_unless_used(state: &AppState, folder: &Path) {
    let settings = state.settings();
    let configured = [
        settings.output_folder.map(PathBuf::from),
        settings.temp_dir.map(PathBuf::from),
        state.config.temp_dir.clone(),
    ];
    if configured.iter().flatten().any(|f| f == folder) {
        return;
    }
    match db::libraries::list(state.db.pool()).await {
        Ok(libs) if libs.iter().any(|l| Path::new(&l.path) == folder) => return,
        Ok(_) => {}
        // Kept: forgetting the mounts of a folder in use would let it be
        // taken for its share once that is unmounted.
        Err(e) => {
            tracing::debug!(folder = %folder.display(), "could not tell whether the folder is in use: {e}");
            return;
        }
    }
    forget(state, folder).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reason_names_the_mount_point() {
        assert_eq!(
            not_connected(Path::new("/mnt/remotes/nas")),
            "The drive or share mounted at /mnt/remotes/nas isn't connected. Reconnect it, \
             and its conversions continue."
        );
    }
}
