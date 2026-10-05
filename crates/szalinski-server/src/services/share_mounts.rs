//! The drives and shares the folders Szalinski uses are mounted from.
//!
//! An unmounted share leaves its mount point behind as an ordinary folder
//! on the disk below: empty, or holding whatever was there before the
//! share was mounted over it. Taken for the share, it would have the files
//! on the share taken for gone (a job whose new file was being put in place
//! when the server stopped would be settled as "not in place", and its
//! backup put back next to the new file once the share is back), and new
//! files written into it, hidden under the share once it is mounted again.
//! The same goes for another filesystem mounted in the share's place: a
//! tmpfs, or the bare folder bind-mounted onto itself, which is what a
//! Docker bind mount shows when the container started before the host
//! mounted the share (on Unraid, remote shares mounted by Unassigned
//! Devices after the array started).
//!
//! So the mount points each library folder, the output folder and the work
//! folder sit on or under are remembered (`folder_mounts`, read from
//! `/proc/self/mountinfo` without touching any share), with what was
//! mounted there ([`MountIdentity`]: its type, source and the folder of it
//! mounted; not the mount's id or device number, which are handed out
//! again), adding any found mounted whenever the folder is looked at. A
//! folder given as a link is followed (with a time limit) to where it
//! really is, so the share a link leads to is remembered too. While one of
//! the mount points isn't mounted ([`Mounted::No`]), or something else is
//! mounted there ([`Mounted::Different`]), the folder is not connected: its
//! library is offline, jobs that use it wait, nothing is written there,
//! and a job whose new file may have been put in place is never settled on
//! it. A folder's mount points are forgotten only when it stops being used
//! for that: a library removed (or added again), the output folder or the
//! work folder changed in Settings (to another place: saved again as it
//! was, it keeps them). A different drive put there on purpose is taken as
//! the usual one only when the user says so ([`relearn`]).
//!
//! Mount points remembered by an older version, without what was mounted
//! there, have it noted at the first look that finds something mounted
//! there ([`note_unknown`], at start-up and every 15 s until none are left,
//! and at every look at their folder), except the folder below bound onto
//! itself (`slow_fs::shows_folder_below`), which is never taken for the
//! share; with nothing mounted there, their folder is not connected.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use szalinski_core::{FolderSetting, OutputMode};
use szalinski_worker::slow_fs::{self, KnownMount, MountIdentity, MountPoint};
use uuid::Uuid;

use crate::db;
use crate::db::folder_mounts::FolderMount;
use crate::state::{AppState, lock};

/// How long finding out where a folder really is (its links followed) may
/// take before the folder's own path is used instead.
const RESOLVE_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(2)
} else {
    Duration::from_secs(5)
};

/// How long where a folder really is is taken as known before it is found
/// out again (in the background: the last answer is used meanwhile).
const RESOLVED_MAX_AGE: Duration = Duration::from_secs(60);

/// How often mount points remembered without what was mounted there are
/// looked at again (see [`note_unknown_loop`]).
const NOTE_EVERY: Duration = if cfg!(test) {
    Duration::from_millis(100)
} else {
    Duration::from_secs(15)
};

/// The mount points remembered for one folder, with what was mounted there
/// (`None`: not noted yet).
type Remembered = BTreeMap<PathBuf, Option<MountIdentity>>;

/// What is remembered, by folder (loaded from the database on first use).
#[derive(Debug, Default)]
pub struct ShareMounts {
    known: std::sync::Mutex<HashMap<PathBuf, Remembered>>,
    /// Where each folder really is (links followed), and when that was
    /// found out.
    resolved: std::sync::Mutex<HashMap<PathBuf, (PathBuf, Instant)>>,
}

/// Whether the mount points a folder is known to sit on are mounted, with
/// what was mounted there before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mounted {
    /// They all are: these, with any found mounted above it now.
    Yes(Vec<KnownMount>),
    /// This one isn't: the folder is an ordinary folder on the disk below.
    No(PathBuf),
    /// Something else is mounted at this one than before (a tmpfs, another
    /// disk, the bare folder bind-mounted in its place).
    Different(PathBuf),
    /// Can't tell (the list of mounts or the database couldn't be read).
    Unknown,
}

impl Mounted {
    /// The mount point that isn't mounted as it was, if one isn't.
    pub fn mount_point(&self) -> Option<&Path> {
        match self {
            Self::No(mount) | Self::Different(mount) => Some(mount),
            Self::Yes(_) | Self::Unknown => None,
        }
    }

    /// Why the folder can't be used, when that is why.
    pub fn problem(&self) -> Option<String> {
        match self {
            Self::No(mount) => Some(not_connected(mount)),
            Self::Different(mount) => Some(different_drive(mount)),
            Self::Yes(_) | Self::Unknown => None,
        }
    }
}

/// The reason given for a folder on a share that isn't mounted.
pub fn not_connected(mount: &Path) -> String {
    format!(
        "The drive or share mounted at {} isn't connected. Reconnect it, and its conversions \
         continue.",
        mount.display()
    )
}

/// The reason given for a folder on a share with something else mounted in
/// its place.
pub fn different_drive(mount: &Path) -> String {
    format!(
        "A different drive is mounted at {} than before. Reconnect the usual one, or tell \
         Szalinski to use the one there now.",
        mount.display()
    )
}

/// The mount on top at `point` among `now` (not one an automounter mounts
/// on demand).
fn mounted_at<'a>(now: &'a [MountPoint], point: &Path) -> Option<&'a MountPoint> {
    now.iter().rfind(|m| m.path == point && !m.on_demand)
}

/// Whether `known` is mounted among `now`, as it was: [`Mounted::No`] or
/// [`Mounted::Different`] when it isn't, `None` when it is. When what was
/// mounted there isn't known (remembered by an older version), anything
/// mounted there will do but the folder below bound onto itself (see
/// `slow_fs::shows_folder_below`): that is another drive than the share.
fn compare(now: &[MountPoint], known: &KnownMount) -> Option<Mounted> {
    let Some(m) = mounted_at(now, &known.point) else {
        return Some(Mounted::No(known.point.clone()));
    };
    let same = match &known.identity {
        Some(id) => *id == m.identity,
        None => !slow_fs::shows_folder_below(now, m),
    };
    (!same).then(|| Mounted::Different(known.point.clone()))
}

/// What is mounted at `point` now, to be noted as what was mounted there
/// for a mount point remembered without it: `None` when nothing is, or
/// only the folder below bound onto itself (never taken for the share).
fn noteable(now: &[MountPoint], point: &Path) -> Option<MountIdentity> {
    let m = mounted_at(now, point)?;
    (!slow_fs::shows_folder_below(now, m)).then(|| m.identity.clone())
}

/// The mount points of `known` not noted yet with what is mounted there
/// now, where something that may be noted is (see [`noteable`]).
fn unnoted_now(now: &[MountPoint], known: &Remembered) -> Remembered {
    known
        .iter()
        .filter(|(_, identity)| identity.is_none())
        .filter_map(|(point, _)| Some((point.clone(), Some(noteable(now, point)?))))
        .collect()
}

/// The first of `known` that isn't mounted as it was, the outermost first
/// (the share itself, when a mount inside it went with it).
fn first_problem(now: &[MountPoint], known: &Remembered) -> Option<Mounted> {
    let mut points: Vec<KnownMount> = known_mounts(known);
    points.sort_by_key(|k| k.point.components().count());
    points.iter().find_map(|k| compare(now, k))
}

fn known_mounts(known: &Remembered) -> Vec<KnownMount> {
    known
        .iter()
        .map(|(point, identity)| KnownMount {
            point: point.clone(),
            identity: identity.clone(),
        })
        .collect()
}

/// The mount points remembered for `folder` (`None`: the database couldn't
/// be read).
async fn remembered(state: &AppState, folder: &Path, key: &str) -> Option<Remembered> {
    let cached = lock(&state.mounts.known).get(folder).cloned();
    if let Some(known) = cached {
        return Some(known);
    }
    match db::folder_mounts::get(state.db.pool(), key).await {
        Ok(rows) => {
            let rows: Remembered = rows
                .into_iter()
                .map(|r| (PathBuf::from(r.mount), r.identity))
                .collect();
            let mut known = lock(&state.mounts.known);
            let entry = known.entry(folder.to_path_buf()).or_default();
            for (point, identity) in &rows {
                let noted = entry.entry(point.clone()).or_default();
                if noted.is_none() {
                    noted.clone_from(identity);
                }
            }
            Some(entry.clone())
        }
        Err(e) => {
            tracing::debug!(folder = %folder.display(), "could not read the folder's mounts: {e}");
            None
        }
    }
}

/// Whether the mount points `folder` is known to sit on are all mounted as
/// they were, without learning anything new or touching any disk.
/// What was mounted at a mount point remembered without it is noted when
/// something that may be is mounted there now (see [`noteable`]).
pub async fn status(state: &AppState, folder: &Path) -> Mounted {
    let Some(key) = folder.to_str() else {
        return Mounted::Yes(Vec::new());
    };
    let Some(known) = remembered(state, folder, key).await else {
        return Mounted::Unknown;
    };
    if known.is_empty() {
        return Mounted::Yes(Vec::new());
    }
    let Some(now) = slow_fs::mount_points().await else {
        return Mounted::Unknown;
    };
    if let Some(problem) = first_problem(&now, &known) {
        return problem;
    }
    let noted = unnoted_now(&now, &known);
    let mut all = known;
    if !noted.is_empty() {
        all.extend(noted.iter().map(|(p, i)| (p.clone(), i.clone())));
        save(state, folder, key, &noted).await;
    }
    Mounted::Yes(known_mounts(&all))
}

/// Whether the mount points `folder` is known to sit on (at or above it,
/// or above where it really is when it is a link) are all mounted, with
/// what was mounted there before; when they are, any others mounted there
/// now are remembered too, and what is mounted at one not noted yet is
/// noted.
pub async fn check(state: &AppState, folder: &Path) -> Mounted {
    check_learning(state, folder, false).await
}

/// [`check`] for the output folder, whose folders the new files go into:
/// drives and shares mounted inside it are remembered too (`out/Movies` on
/// a share of its own), so a job doesn't write into such a mount point once
/// its share is unmounted. (A library folder's own mounts are looked after
/// by its scans, file by file, and the work folder's temp files go into
/// the folder itself.)
pub async fn check_output(state: &AppState, folder: &Path) -> Mounted {
    check_learning(state, folder, true).await
}

async fn check_learning(state: &AppState, folder: &Path, inside: bool) -> Mounted {
    // A folder that can't be stored as text is never remembered.
    let Some(key) = folder.to_str() else {
        return Mounted::Yes(Vec::new());
    };
    let Some(known) = remembered(state, folder, key).await else {
        return Mounted::Unknown;
    };
    let Some(now) = slow_fs::mount_points().await else {
        return if known.is_empty() {
            Mounted::Yes(Vec::new())
        } else {
            Mounted::Unknown
        };
    };
    if let Some(problem) = first_problem(&now, &known) {
        return problem;
    }
    let real = resolve(state, folder).await;
    // Not "/", which is always there, nor a share an automounter mounts on
    // demand (it comes and goes by itself, and a look at its folder mounts
    // it again; it leaves no ordinary folder behind).
    let above: Remembered = now
        .iter()
        .filter(|m| {
            !m.on_demand
                && (folder.starts_with(&m.path)
                    || real.starts_with(&m.path)
                    || (inside && (m.path.starts_with(folder) || m.path.starts_with(&real))))
                && m.path.parent().is_some()
                && m.path.to_str().is_some()
        })
        .map(|m| (m.path.clone(), Some(m.identity.clone())))
        .collect();
    // New ones, and ones remembered without what was mounted there (never
    // the folder below bound onto itself: the share isn't there).
    let mut learned: Remembered = above
        .iter()
        .filter(|(point, _)| !known.contains_key(*point))
        .map(|(point, identity)| (point.clone(), identity.clone()))
        .collect();
    learned.extend(unnoted_now(&now, &known));
    let mut all = known;
    if !learned.is_empty() {
        all.extend(learned.iter().map(|(p, i)| (p.clone(), i.clone())));
        save(state, folder, key, &learned).await;
    }
    Mounted::Yes(known_mounts(&all))
}

/// Remember `learned` for `folder` (stored as `key`): new mount points, and
/// what is mounted at ones remembered without it.
async fn save(state: &AppState, folder: &Path, key: &str, learned: &Remembered) {
    // Only into what is loaded already: a folder not loaded yet is read
    // whole from the database at its first look (an entry made here would
    // hold these mount points alone, and the folder's others would be
    // taken as not remembered).
    if let Some(entry) = lock(&state.mounts.known).get_mut(folder) {
        for (point, identity) in learned {
            let noted = entry.entry(point.clone()).or_default();
            if noted.is_none() {
                noted.clone_from(identity);
            }
        }
    }
    let rows: Vec<FolderMount> = learned
        .iter()
        .filter_map(|(point, identity)| {
            Some(FolderMount {
                mount: point.to_str()?.to_string(),
                identity: identity.clone(),
            })
        })
        .collect();
    let saved = async {
        let mut tx = state.db.write_tx().await?;
        db::folder_mounts::add(&mut tx, key, &rows).await?;
        tx.commit().await
    }
    .await;
    match saved {
        Ok(()) => {
            tracing::debug!(folder = %folder.display(), mounts = ?learned, "remembered the folder's mounts")
        }
        Err(e) => {
            tracing::warn!(folder = %folder.display(), "could not record the folder's mounts: {e}")
        }
    }
}

/// Note what is mounted at every mount point remembered without it (by an
/// older version), where something that may be noted is mounted now (see
/// [`noteable`]): the share, when it is there at the first look. Done at
/// start-up, before anything looks at a folder, and every
/// [`NOTE_EVERY`] after ([`note_unknown_loop`]), so it is noted promptly
/// even while nothing else looks at that folder: another drive put there
/// later is then told apart. Whether any are left without it.
pub async fn note_unknown(state: &AppState) -> bool {
    let rows = match db::folder_mounts::unnoted(state.db.pool()).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::debug!("could not read the mount points to note: {e}");
            return true;
        }
    };
    if rows.is_empty() {
        return false;
    }
    let Some(now) = slow_fs::mount_points().await else {
        return true;
    };
    let mut by_folder: BTreeMap<String, Remembered> = BTreeMap::new();
    let mut left = 0;
    for (folder, mount) in rows {
        match noteable(&now, Path::new(&mount)) {
            Some(identity) => {
                tracing::info!(
                    folder = %folder,
                    mount = %mount,
                    "noted what is mounted at a mount point remembered before ({identity})"
                );
                by_folder
                    .entry(folder)
                    .or_default()
                    .insert(PathBuf::from(mount), Some(identity));
            }
            None => left += 1,
        }
    }
    for (folder, learned) in &by_folder {
        save(state, Path::new(folder), folder, learned).await;
    }
    left > 0
}

/// [`note_unknown`] every [`NOTE_EVERY`] until none are left (or the
/// server shuts down).
pub async fn note_unknown_loop(state: AppState) {
    loop {
        tokio::select! {
            () = state.shutdown.cancelled() => break,
            () = tokio::time::sleep(NOTE_EVERY) => {}
        }
        if !note_unknown(&state).await {
            break;
        }
    }
}

/// Take what is mounted now at each mount point remembered for `folder`
/// with something else mounted there than before as the usual one from now
/// on (the user put another drive there on purpose). Mount points that
/// aren't mounted at all stay as they are: their folder is still not
/// connected. Returns the mount points taken anew.
pub async fn relearn(state: &AppState, folder: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let Some(key) = folder.to_str() else {
        return Ok(Vec::new());
    };
    let Some(known) = remembered(state, folder, key).await else {
        anyhow::bail!("could not read the folder's mounts");
    };
    let Some(now) = slow_fs::mount_points().await else {
        anyhow::bail!("could not read the list of mounts");
    };
    let changed: Vec<(PathBuf, MountIdentity)> = known_mounts(&known)
        .into_iter()
        .filter_map(|known| {
            let Some(Mounted::Different(point)) = compare(&now, &known) else {
                return None;
            };
            let m = mounted_at(&now, &point)?;
            Some((point, m.identity.clone()))
        })
        .collect();
    if !changed.is_empty() {
        let mut tx = state.db.write_tx().await?;
        for (point, identity) in &changed {
            if let Some(mount) = point.to_str() {
                db::folder_mounts::set_identity(&mut tx, key, mount, identity).await?;
            }
        }
        tx.commit().await?;
        let mut cache = lock(&state.mounts.known);
        let entry = cache.entry(folder.to_path_buf()).or_default();
        for (point, identity) in &changed {
            entry.insert(point.clone(), Some(identity.clone()));
            tracing::info!(
                folder = %folder.display(),
                mount = %point.display(),
                "using the drive mounted there now ({identity})"
            );
        }
    }
    Ok(changed.into_iter().map(|(point, _)| point).collect())
}

/// Whether `known` (a mount a job's new file goes into) is mounted now as it
/// was then: [`Mounted::Yes`] with it, [`Mounted::No`] or
/// [`Mounted::Different`] when it isn't, [`Mounted::Unknown`] when the
/// list of mounts can't be read.
pub async fn still_mounted(known: &KnownMount) -> Mounted {
    let Some(now) = slow_fs::mount_points().await else {
        return Mounted::Unknown;
    };
    compare(&now, known).unwrap_or_else(|| Mounted::Yes(vec![known.clone()]))
}

/// The mount `path` is on now (where it really is, with its links
/// followed within a time limit), with what is mounted there: the longest
/// mount point above it, but not `/` nor a share an automounter mounts on
/// demand. `None` when it is on neither, or the list of mounts can't be
/// read.
pub async fn mount_of(path: &Path) -> Option<KnownMount> {
    let real = slow_fs::real_path(path, RESOLVE_TIMEOUT).await;
    let now = slow_fs::mount_points().await?;
    let on = |p: &Path| {
        now.iter()
            .filter(|m| !m.on_demand && m.path.parent().is_some() && p.starts_with(&m.path))
            .max_by_key(|m| m.path.components().count())
            .map(|m| KnownMount {
                point: m.path.clone(),
                identity: Some(m.identity.clone()),
            })
    };
    real.as_deref().and_then(on).or_else(|| on(path))
}

/// Where `folder` really is, its links followed: found out with a time
/// limit the first time, then taken from the last answer (and found out
/// again in the background once that is old). The folder's own path when
/// that can't be told.
async fn resolve(state: &AppState, folder: &Path) -> PathBuf {
    let cached = lock(&state.mounts.resolved).get(folder).cloned();
    match cached {
        Some((real, at)) => {
            if at.elapsed() > RESOLVED_MAX_AGE {
                // Found out again once, in the background.
                lock(&state.mounts.resolved)
                    .insert(folder.to_path_buf(), (real.clone(), Instant::now()));
                let (state, folder) = (state.clone(), folder.to_path_buf());
                tokio::spawn(async move {
                    if let Some(found) = slow_fs::real_path(&folder, RESOLVE_TIMEOUT).await {
                        lock(&state.mounts.resolved).insert(folder, (found, Instant::now()));
                    }
                });
            }
            real
        }
        None => match slow_fs::real_path(folder, RESOLVE_TIMEOUT).await {
            Some(real) => {
                lock(&state.mounts.resolved)
                    .insert(folder.to_path_buf(), (real.clone(), Instant::now()));
                real
            }
            // Not known (the folder doesn't answer): its own path for now,
            // found out again at the next look, in the background, rather
            // than waited for at every look while it doesn't answer.
            None => {
                let stale = Instant::now()
                    .checked_sub(RESOLVED_MAX_AGE + Duration::from_secs(1))
                    .unwrap_or_else(Instant::now);
                lock(&state.mounts.resolved)
                    .insert(folder.to_path_buf(), (folder.to_path_buf(), stale));
                folder.to_path_buf()
            }
        },
    }
}

/// The folders the jobs of the library at `lib_path` use: the library
/// folder (first), the work folder and, in folder mode, the output folder.
pub fn folders_of_jobs(state: &AppState, lib_path: &Path) -> Vec<PathBuf> {
    let settings = state.settings();
    let mut folders = vec![lib_path.to_path_buf()];
    folders.extend(
        settings
            .temp_dir
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| state.config.temp_dir.clone()),
    );
    if settings.output_mode == OutputMode::Folder {
        folders.extend(settings.output_folder.as_deref().map(PathBuf::from));
    }
    folders
}

/// The mount points with something else mounted than before, among those
/// the folders the jobs of library `lib_id` (at `lib_path`) use sit on and
/// those the new files of its conversions whose placing isn't settled go
/// into. Nothing is learned, and no disk is touched.
pub async fn changed_mounts(state: &AppState, lib_id: Uuid, lib_path: &Path) -> Vec<PathBuf> {
    let mut changed = Vec::new();
    for folder in folders_of_jobs(state, lib_path) {
        if let Mounted::Different(mount) = status(state, &folder).await {
            changed.push(mount);
        }
    }
    let placing = db::jobs::placing_final_mounts(state.db.pool(), lib_id)
        .await
        .unwrap_or_default();
    for mount in placing {
        if let Mounted::Different(point) = still_mounted(&mount).await {
            changed.push(point);
        }
    }
    changed.sort();
    changed.dedup();
    changed
}

/// [`relearn`] every folder the jobs of library `lib_id` (at `lib_path`)
/// use, and take what is mounted now as the mount the new files of its
/// conversions whose placing isn't settled go into, where something else
/// was mounted. Returns the mount points taken anew.
pub async fn relearn_library(
    state: &AppState,
    lib_id: Uuid,
    lib_path: &Path,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut taken = Vec::new();
    for folder in folders_of_jobs(state, lib_path) {
        taken.extend(relearn(state, &folder).await?);
    }
    let pool = state.db.pool();
    let placing = db::jobs::placing_final_mounts(pool, lib_id).await?;
    if !placing.is_empty() {
        let Some(now) = slow_fs::mount_points().await else {
            anyhow::bail!("could not read the list of mounts");
        };
        for mount in placing {
            if let Some(Mounted::Different(point)) = compare(&now, &mount)
                && let Some(m) = mounted_at(&now, &point)
            {
                db::jobs::relearn_final_mounts(pool, lib_id, &point, &m.identity).await?;
                taken.push(point);
            }
        }
    }
    taken.sort();
    taken.dedup();
    Ok(taken)
}

/// Forget what is known about `folder` (it stops being used, or is chosen
/// again): its mounts are learned afresh from the next look at it.
pub async fn forget(state: &AppState, folder: &Path) {
    lock(&state.mounts.known).remove(folder);
    lock(&state.mounts.resolved).remove(folder);
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

/// Whether `folder` is used for something else than the folder setting
/// `setting`: a library folder, or the other of the output and work
/// folders. Taken as used when that can't be told (the libraries can't be
/// read): what is remembered for a folder in use is never forgotten.
pub async fn used_besides(state: &AppState, folder: &Path, setting: FolderSetting) -> bool {
    let settings = state.settings();
    let other = match setting {
        FolderSetting::OutputFolder => settings
            .temp_dir
            .map(PathBuf::from)
            .or_else(|| state.config.temp_dir.clone()),
        FolderSetting::TempDir => settings.output_folder.map(PathBuf::from),
    };
    if other.as_deref() == Some(folder) {
        return true;
    }
    match db::libraries::list(state.db.pool()).await {
        Ok(libs) => libs.iter().any(|l| Path::new(&l.path) == folder),
        Err(e) => {
            tracing::debug!(folder = %folder.display(), "could not tell whether the folder is in use: {e}");
            true
        }
    }
}

/// Whether the folder settings `before` (saved) and `after` (being saved)
/// name the same folder: the same path, or paths that lead to the same
/// place once links are followed (each found out with a time limit; one
/// that doesn't answer in time counts as where it was last found to lead,
/// or as given).
pub async fn same_folder(state: &AppState, before: Option<&str>, after: Option<&str>) -> bool {
    match (before, after) {
        (None, None) => true,
        (Some(before), Some(after)) => same_place(state, Path::new(before), Path::new(after)).await,
        _ => false,
    }
}

async fn same_place(state: &AppState, before: &Path, after: &Path) -> bool {
    if before == after {
        return true;
    }
    let (was, now) = tokio::join!(
        slow_fs::real_path(before, RESOLVE_TIMEOUT),
        slow_fs::real_path(after, RESOLVE_TIMEOUT),
    );
    let was = was
        .or_else(|| {
            lock(&state.mounts.resolved)
                .get(before)
                .map(|(real, _)| real.clone())
        })
        .unwrap_or_else(|| before.to_path_buf());
    let now = now.unwrap_or_else(|| after.to_path_buf());
    was == now
}

/// What is remembered for `from` is remembered for `to` too (the same
/// folder, given another way: through a link, say).
pub async fn carry_over(state: &AppState, from: &Path, to: &Path) {
    let (Some(from_key), Some(to_key)) = (from.to_str(), to.to_str()) else {
        return;
    };
    let Some(known) = remembered(state, from, from_key).await else {
        tracing::warn!(folder = %from.display(), "could not read the folder's mounts to keep them");
        return;
    };
    if !known.is_empty() {
        save(state, to, to_key, &known).await;
    }
}

/// Whether the drives and shares `folder` is known to sit on aren't
/// mounted as they were (not connected, or another drive in a share's
/// place), so nothing may be written there.
pub async fn away(state: &AppState, folder: &Path) -> bool {
    matches!(
        status(state, folder).await,
        Mounted::No(_) | Mounted::Different(_)
    )
}

/// The output folder (in folder mode) and the work folder in use (the one
/// chosen in Settings, else the one Szalinski was started with), if any.
pub fn configured_folders(state: &AppState) -> Vec<(FolderSetting, PathBuf)> {
    let settings = state.settings();
    let mut folders = Vec::new();
    if settings.output_mode == OutputMode::Folder
        && let Some(out) = settings.output_folder
    {
        folders.push((FolderSetting::OutputFolder, PathBuf::from(out)));
    }
    if let Some(work) = settings
        .temp_dir
        .map(PathBuf::from)
        .or_else(|| state.config.temp_dir.clone())
    {
        folders.push((FolderSetting::TempDir, work));
    }
    folders
}

/// Take what is mounted now at each of `points` (places where the user
/// said to use the drive there now) as the mount the new files of every
/// conversion whose placing isn't settled go into, where that was there.
pub async fn relearn_placing_at(state: &AppState, points: &[PathBuf]) -> anyhow::Result<()> {
    if points.is_empty() {
        return Ok(());
    }
    let Some(now) = slow_fs::mount_points().await else {
        anyhow::bail!("could not read the list of mounts");
    };
    let pool = state.db.pool();
    for lib in db::libraries::list(pool).await? {
        for point in points {
            if let Some(m) = mounted_at(&now, point) {
                db::jobs::relearn_final_mounts(pool, lib.id, point, &m.identity).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reasons_name_the_mount_point() {
        assert_eq!(
            not_connected(Path::new("/mnt/remotes/nas")),
            "The drive or share mounted at /mnt/remotes/nas isn't connected. Reconnect it, \
             and its conversions continue."
        );
        assert_eq!(
            different_drive(Path::new("/mnt/remotes/nas")),
            "A different drive is mounted at /mnt/remotes/nas than before. Reconnect the usual \
             one, or tell Szalinski to use the one there now."
        );
    }

    fn point(path: &str, fstype: &str) -> MountPoint {
        MountPoint {
            path: PathBuf::from(path),
            on_demand: false,
            identity: MountIdentity {
                fstype: fstype.to_string(),
                source: "src".to_string(),
                root: "/".to_string(),
            },
        }
    }

    /// The outermost problem is the one told: a share gone with a mount
    /// inside it is the share; another filesystem in a share's place is
    /// told apart from a share that isn't mounted.
    #[test]
    fn the_outermost_problem_is_told() {
        let nfs = point("/mnt/nas", "nfs4").identity;
        let usb = point("/mnt/nas/usb", "vfat").identity;
        let known: Remembered = [
            (PathBuf::from("/mnt/nas"), Some(nfs)),
            (PathBuf::from("/mnt/nas/usb"), Some(usb)),
        ]
        .into_iter()
        .collect();
        let all = [
            point("/", "ext4"),
            point("/mnt/nas", "nfs4"),
            point("/mnt/nas/usb", "vfat"),
        ];
        assert_eq!(first_problem(&all, &known), None);
        assert_eq!(
            first_problem(&all[..1], &known),
            Some(Mounted::No(PathBuf::from("/mnt/nas")))
        );
        let other = [point("/", "ext4"), point("/mnt/nas", "tmpfs")];
        assert_eq!(
            first_problem(&other, &known),
            Some(Mounted::Different(PathBuf::from("/mnt/nas")))
        );
        let inner_other = [
            point("/", "ext4"),
            point("/mnt/nas", "nfs4"),
            point("/mnt/nas/usb", "ext4"),
        ];
        assert_eq!(
            first_problem(&inner_other, &known),
            Some(Mounted::Different(PathBuf::from("/mnt/nas/usb")))
        );
    }

    /// A mount point remembered without what was mounted there (by an
    /// older version): with nothing mounted it is not connected; the folder
    /// below bound onto itself is another drive, never noted as the share;
    /// anything else there is taken, and noted.
    #[test]
    fn a_place_remembered_without_its_drive() {
        let known: Remembered = [(PathBuf::from("/mnt/nas"), None)].into_iter().collect();
        let on_disk = |path: &str, root: &str| MountPoint {
            path: PathBuf::from(path),
            on_demand: false,
            identity: MountIdentity {
                fstype: "ext4".to_string(),
                source: "/dev/vda".to_string(),
                root: root.to_string(),
            },
        };
        let root = on_disk("/", "/");
        let bare = on_disk("/mnt/nas", "/mnt/nas");
        let share = point("/mnt/nas", "nfs4");

        let gone = [root.clone()];
        assert_eq!(
            first_problem(&gone, &known),
            Some(Mounted::No(PathBuf::from("/mnt/nas")))
        );
        assert!(unnoted_now(&gone, &known).is_empty());

        let bound = [root.clone(), bare.clone()];
        assert_eq!(
            first_problem(&bound, &known),
            Some(Mounted::Different(PathBuf::from("/mnt/nas")))
        );
        assert!(unnoted_now(&bound, &known).is_empty(), "never noted");

        // The share mounted over the bound folder (listed after it).
        let back = [root, bare, share.clone()];
        assert_eq!(first_problem(&back, &known), None);
        assert_eq!(
            unnoted_now(&back, &known),
            [(PathBuf::from("/mnt/nas"), Some(share.identity))]
                .into_iter()
                .collect::<Remembered>()
        );
    }
}
