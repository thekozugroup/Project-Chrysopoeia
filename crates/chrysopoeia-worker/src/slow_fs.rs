//! Filesystem calls on folders that may stop answering.
//!
//! On a network share whose server went away (an NFS hard mount) or a stuck
//! FUSE mount, every system call on the folder blocks, sometimes for good.
//! `tokio::fs` runs each call on a blocking thread, and giving up on it with
//! a timeout leaves that thread stuck; repeated on every page load or every
//! retry of a job, stuck threads pile up until the runtime's blocking pool
//! (512 threads) is used up and everything that touches a disk stops,
//! healthy folders included.
//!
//! [`guarded`] runs one check per path (and kind of check) at a time: a
//! caller that comes while the same check is still running waits for its
//! answer, up to its own timeout, instead of starting another. Checks are
//! counted by the mount they are on (the longest mount point in
//! `/proc/self/mountinfo` above the path, which is read without touching the
//! share; see [`group_of`]):
//!
//! - at most [`MAX_STUCK_PER_MOUNT`] run at once on one mount. Once that many
//!   are stuck there (each has run past its caller's timeout), a further
//!   check of anything in the folder they have in common counts as not
//!   answering straight away, without a thread, so one share that stopped
//!   answering costs a few threads at most, however many different folders
//!   on it are looked at. A check elsewhere on a full mount, away from the
//!   folder its slow checks have in common, may take room beyond that while
//!   fewer than that many checks there are not slow: what is slow there is
//!   then another share counted with this mount (one mounted since the list
//!   of mounts was read, or reached through a link), and checks stuck on it
//!   never take the room of the rest of the mount;
//! - a check is counted by the mount it started on for as long as it runs.
//!   Mounts are told apart by their id, so a share mounted again at the
//!   same place is another mount. When the list of mounts is read again
//!   (whenever a check gets no answer in time or finds no room on its
//!   mount, at most once a second), only a mount that appeared since,
//!   between the one a check is counted by and its path, takes it over: a
//!   share mounted after the list was read soon has its stuck checks
//!   counted by its own mount. A check whose mount is gone from the list (a
//!   hung share unmounted lazily, `umount -l`) stays counted by that mount:
//!   its stuck checks are charged to no other;
//! - at most [`MAX_STUCK_CHECKS`] run at once in all, which leaves room for
//!   healthy folders while many shares hang. A check that can't start for
//!   that reason is [`NoAnswer::Busy`]: nothing is known about its folder,
//!   and it is never taken for one that stopped answering.
//!
//! [`first_unmounted`] tells whether the mount points a job's folders sit
//! on are all still mounted (an unmounted share leaves its mount point
//! behind as an ordinary folder on the disk below).
//!
//! The server and the worker share these checks.

use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[cfg(not(any(test, feature = "test-hooks")))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::watch;

/// Checks that may run at once in all (each on a blocking thread): room for
/// [`MAX_STUCK_PER_MOUNT`] stuck checks on each of 16 shares that stopped
/// answering, far below the runtime's 512 blocking threads.
pub const MAX_STUCK_CHECKS: usize = 128;

/// Checks that may run at once on one mount (see [`group_of`]).
pub const MAX_STUCK_PER_MOUNT: usize = 8;

/// How long a file or folder a job works with may take to answer before
/// it counts as not responding. Generous: an array disk that has to spin
/// up first takes several seconds.
pub const FOLDER_CHECK_TIMEOUT: Duration = if cfg!(any(test, feature = "test-hooks")) {
    Duration::from_secs(2)
} else {
    Duration::from_secs(30)
};

/// How often the files and folders of a long step (ffprobe, an encode, the
/// checks, putting the new file in place) are looked at while it runs. A
/// share that stops answering is noticed within this plus
/// [`FOLDER_CHECK_TIMEOUT`]; one look every few seconds at a disk that is
/// being read anyway costs nothing.
pub const WATCH_INTERVAL: Duration = if cfg!(any(test, feature = "test-hooks")) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(5)
};

/// How often a check that waits for room to start tries again.
const SLOT_RETRY: Duration = Duration::from_millis(50);

/// How long the list of mount points is used before it is read again in
/// the background (a share mounted or unmounted meanwhile). It is read
/// again sooner when a check runs into trouble (see [`refresh_mounts`]).
#[cfg(not(any(test, feature = "test-hooks")))]
const MOUNTS_MAX_AGE: Duration = Duration::from_secs(30);

/// How soon the list of mount points may be read again when a check runs
/// into trouble (no answer in time, or no room on its mount).
#[cfg(not(any(test, feature = "test-hooks")))]
const MOUNTS_MIN_AGE: Duration = Duration::from_secs(1);

/// How long a check runs before it counts as slow (see [`Slots::take`]):
/// far longer than a look at a folder that answers takes.
const SLOW_CHECK: Duration = if cfg!(any(test, feature = "test-hooks")) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(2)
};

/// Why a [`guarded`] check gave no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoAnswer {
    /// It didn't answer in time: the share or drive it is on isn't
    /// responding.
    NotAnswering,
    /// It wasn't made: checks of shares that stopped answering hold every
    /// thread set aside for checks. Nothing is known about the folder; try
    /// again shortly.
    Busy,
}

impl NoAnswer {
    /// This, about `path`.
    pub fn at(self, path: &Path) -> NotAnswering {
        NotAnswering {
            path: path.to_path_buf(),
            busy: self == Self::Busy,
        }
    }
}

/// A file or folder that gave no answer: it is on a share or drive that
/// stopped responding, or (`busy`) it couldn't be looked at right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAnswering {
    /// What didn't answer.
    pub path: PathBuf,
    /// It wasn't looked at (see [`NoAnswer::Busy`]): nothing is known about
    /// it, so it must not be taken for one that stopped answering.
    pub busy: bool,
}

impl NotAnswering {
    /// `path` didn't answer.
    pub fn at(path: &Path) -> Self {
        NoAnswer::NotAnswering.at(path)
    }
}

impl std::fmt::Display for NotAnswering {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.busy {
            write!(f, "{} couldn't be checked right now", self.path.display())
        } else {
            write!(f, "{} isn't responding", self.path.display())
        }
    }
}

impl std::error::Error for NotAnswering {}

type Answer = Arc<dyn Any + Send + Sync>;
type Flights = HashMap<(&'static str, PathBuf), watch::Receiver<Option<Answer>>>;

static FLIGHTS: LazyLock<Mutex<Flights>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static SLOTS: LazyLock<Mutex<Slots>> = LazyLock::new(|| Mutex::new(Slots::default()));

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How many checks may run at once.
#[derive(Debug, Clone, Copy)]
struct Limits {
    per_group: usize,
    total: usize,
}

const LIMITS: Limits = Limits {
    per_group: MAX_STUCK_PER_MOUNT,
    total: MAX_STUCK_CHECKS,
};

/// The checks running now, and the mount points they are counted by.
#[derive(Debug, Default)]
struct Slots {
    running: Vec<Slot>,
    next_id: u64,
    /// The mounts checks are counted by (see [`group_in`]).
    mounts: Arc<Vec<Mount>>,
    /// When they were read (`None`: not yet).
    mounts_read: Option<Instant>,
}

/// A mount checks are counted by.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mount {
    /// Its id in `/proc/self/mountinfo`, which tells it from another mount
    /// at the same place (a share unmounted, and another one mounted there
    /// since); `None` for a path no listed mount is above (see
    /// [`group_in`]).
    id: Option<u64>,
    /// Where it is mounted (or the path's first folders).
    point: PathBuf,
}

/// One running check.
#[derive(Debug)]
struct Slot {
    id: u64,
    /// What it looks at.
    path: PathBuf,
    /// The mount it is counted by: the one it started on, or one mounted
    /// between that and its path since (see [`taken_over`]).
    group: Mount,
    started: Instant,
    /// When the caller that started it gave up waiting for it: from then
    /// on it counts as stuck.
    overdue_at: Instant,
}

impl Slot {
    /// Whether it has run for [`SLOW_CHECK`] or is stuck at `now`.
    fn is_slow(&self, now: Instant) -> bool {
        self.started + SLOW_CHECK <= now || self.overdue_at <= now
    }
}

/// Why a check couldn't start now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refused {
    /// Its mount has as many stuck checks as it may, and it is in the
    /// folder they have in common: that isn't answering.
    GroupStuck,
    /// Its mount has as many checks as it may (and no room is left beyond
    /// that for it), not all stuck yet.
    GroupFull,
    /// As many checks as may run in all are running, on other mounts.
    AllBusy,
}

impl Slots {
    /// The mount checks of `path` are counted by.
    fn group(&self, path: &Path) -> Mount {
        group_in(&self.mounts, path, FALLBACK_COMPONENTS)
    }

    /// The checks running on `group`.
    fn on(&self, group: &Mount) -> Vec<&Slot> {
        self.running.iter().filter(|s| s.group == *group).collect()
    }

    /// Room for a check of `path`, started now and given up on at
    /// `overdue_at`: its slot's id.
    ///
    /// A mount holds [`Limits::per_group`] checks. Once that many are stuck
    /// there, a check of anything in the folder they have in common is
    /// refused at once (`GroupStuck`). A check elsewhere on a full mount,
    /// away from the folder its slow checks (stuck, or running for
    /// [`SLOW_CHECK`]) have in common, may take room beyond that, as long
    /// as fewer than [`Limits::per_group`] checks there are not slow: what
    /// is slow there is then most likely another share counted with this
    /// mount (one mounted since the list of mounts was read, or reached
    /// through a link), and must not stop the rest of the mount for as long
    /// as it hangs, however many of its checks are stuck. Each such share
    /// costs a few threads at most: a check of it beyond its room is in
    /// the folder its slow checks have in common, or once those are slow
    /// too, in the folder all of them have in common, which takes in more
    /// with each new folder they are in.
    fn take(
        &mut self,
        path: &Path,
        now: Instant,
        overdue_at: Instant,
        limits: Limits,
    ) -> Result<u64, Refused> {
        let group = self.group(path);
        let on_mount = self.on(&group);
        if stuck_among(&on_mount, path, now, limits) {
            return Err(Refused::GroupStuck);
        }
        if on_mount.len() >= limits.per_group {
            let (slow, fresh): (Vec<&Slot>, Vec<&Slot>) =
                on_mount.iter().partition(|s| s.is_slow(now));
            let slow: Vec<&Path> = slow.iter().map(|s| s.path.as_path()).collect();
            let elsewhere =
                slow.len() >= limits.per_group && !path.starts_with(common_folder(&slow));
            if !elsewhere || fresh.len() >= limits.per_group {
                return Err(Refused::GroupFull);
            }
        }
        if self.running.len() >= limits.total {
            return Err(Refused::AllBusy);
        }
        self.next_id += 1;
        let id = self.next_id;
        self.running.push(Slot {
            id,
            path: path.to_path_buf(),
            group,
            started: now,
            overdue_at,
        });
        Ok(id)
    }

    /// Whether `path` is in the folder the stuck checks of its mount have
    /// in common, and they are as many as the mount may hold.
    fn is_stuck(&self, path: &Path, now: Instant, limits: Limits) -> bool {
        stuck_among(&self.on(&self.group(path)), path, now, limits)
    }

    /// The check in slot `id` ended.
    fn give_back(&mut self, id: u64) {
        self.running.retain(|s| s.id != id);
    }

    /// Count new checks by `mounts` from now on, and the running ones that
    /// a mount which appeared since takes over (see [`taken_over`]).
    /// `read_at`: when the list was read.
    fn set_mounts(&mut self, mounts: Vec<Mount>, read_at: Instant) {
        self.mounts_read = Some(read_at);
        if *self.mounts == mounts {
            return;
        }
        let before = std::mem::replace(&mut self.mounts, Arc::new(mounts));
        let now = Arc::clone(&self.mounts);
        for slot in &mut self.running {
            if let Some(group) = taken_over(&before, &now, slot) {
                slot.group = group;
            }
        }
    }
}

/// The mount that takes `slot` over now that the list of mounts went from
/// `before` to `now`, if any: one that wasn't listed before, between the
/// mount the slot is counted by (still listed) and its path. That is the
/// share the check looks at, mounted after the list was read, and counting
/// its stuck checks by it gives the mount above its room back. Nothing
/// else moves a check: one whose mount is gone from the list (a share
/// unmounted lazily while its checks hang) stays counted by it, never by
/// the mount above, where it would take the room of healthy folders for as
/// long as the share hangs; and a share mounted again at the same place is
/// another mount, which the checks stuck on the old one aren't counted by.
fn taken_over(before: &[Mount], now: &[Mount], slot: &Slot) -> Option<Mount> {
    let candidate = group_in(now, &slot.path, FALLBACK_COMPONENTS);
    let appeared = candidate.id.is_some() && !before.contains(&candidate);
    let below = match slot.group.id {
        Some(_) => {
            now.contains(&slot.group)
                && candidate.point != slot.group.point
                && candidate.point.starts_with(&slot.group.point)
        }
        // Counted by its first folders (no mount was listed above it).
        None => candidate.point.starts_with(&slot.group.point),
    };
    (appeared && below).then_some(candidate)
}

/// Whether, among the checks of one mount (`on_mount`), as many are stuck
/// at `now` as the mount may hold, and `path` is in the folder they have in
/// common.
fn stuck_among(on_mount: &[&Slot], path: &Path, now: Instant, limits: Limits) -> bool {
    let stuck: Vec<&Path> = on_mount
        .iter()
        .filter(|s| s.overdue_at <= now)
        .map(|s| s.path.as_path())
        .collect();
    stuck.len() >= limits.per_group && path.starts_with(common_folder(&stuck))
}

/// The longest path all of `paths` start with, component by component.
fn common_folder(paths: &[&Path]) -> PathBuf {
    let Some((first, rest)) = paths.split_first() else {
        return PathBuf::new();
    };
    let mut common: Vec<std::path::Component<'_>> = first.components().collect();
    for p in rest {
        let same = common
            .iter()
            .zip(p.components())
            .take_while(|(a, b)| **a == *b)
            .count();
        common.truncate(same);
    }
    common.iter().collect()
}

/// A running check's slot, given back when the check ends (on its thread).
struct SlotGuard(u64);

impl Drop for SlotGuard {
    fn drop(&mut self) {
        lock(&SLOTS).give_back(self.0);
    }
}

/// A running check's flight, forgotten when the check ends (on its thread,
/// even if it panics: later callers then start a fresh check instead of
/// waiting for an answer that never comes).
struct FlightGuard((&'static str, PathBuf));

impl Drop for FlightGuard {
    fn drop(&mut self) {
        lock(&FLIGHTS).remove(&self.0);
    }
}

/// How a [`guarded`] check ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Checked<T> {
    Answered(T),
    /// No answer within the timeout, or its mount has as many stuck checks
    /// as it may.
    TimedOut,
    /// It couldn't start: as many checks as may run are running elsewhere.
    Busy,
}

/// Run `work` (blocking filesystem calls on `path`) on a blocking thread and
/// wait up to `timeout` for its answer. `kind` names the check, so different
/// checks of one path don't share answers; one `kind` always answers with
/// the same type.
pub async fn guarded<T>(
    kind: &'static str,
    path: &Path,
    timeout: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, NoAnswer>
where
    T: Clone + Send + Sync + 'static,
{
    match checked(kind, path, timeout, work).await {
        Checked::Answered(answer) => Ok(answer),
        Checked::TimedOut => Err(NoAnswer::NotAnswering),
        Checked::Busy => Err(NoAnswer::Busy),
    }
}

async fn checked<T>(
    kind: &'static str,
    path: &Path,
    timeout: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Checked<T>
where
    T: Clone + Send + Sync + 'static,
{
    if hang::is_busy(path) {
        return Checked::Busy;
    }
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + timeout;
    let key = (kind, path.to_path_buf());
    current_mounts().await;
    let mut work = Some(work);
    let mut rx = loop {
        // Join the check of the same thing under way, or take room for a
        // new one; no lock is held once this block ends.
        let started_flight = {
            let mut flights = lock(&FLIGHTS);
            if let Some(rx) = flights.get(&key) {
                // Its mount has as many stuck checks as it may (this one
                // among them, likely): it isn't answering.
                if lock(&SLOTS).is_stuck(path, Instant::now(), LIMITS) {
                    return Checked::TimedOut;
                }
                break rx.clone();
            }
            let taken = lock(&SLOTS).take(path, Instant::now(), started + timeout, LIMITS);
            taken.map(|id| {
                let (tx, rx) = watch::channel(None);
                flights.insert(key.clone(), rx.clone());
                (id, tx, rx)
            })
        };
        match started_flight {
            Ok((id, tx, rx)) => {
                let slot = SlotGuard(id);
                let flight = FlightGuard(key.clone());
                // Taken only here, and the loop ends with it.
                let Some(work) = work.take() else {
                    return Checked::TimedOut;
                };
                tokio::task::spawn_blocking(move || {
                    hang::wait_while_hung(&flight.0.1);
                    let answer: Answer = Arc::new(work());
                    // Forget the flight first: a caller arriving after this
                    // starts a fresh check, one arriving before gets this
                    // answer.
                    drop(flight);
                    let _ = tx.send(Some(answer));
                    drop(slot);
                });
                break rx;
            }
            Err(Refused::GroupStuck) => {
                tracing::debug!(path = %path.display(), "too many checks of its mount are stuck");
                return Checked::TimedOut;
            }
            Err(refused) => {
                // A full mount may be one whose list is out of date (a
                // share mounted since it was read, counted with the mount
                // above it).
                refresh_mounts();
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    return if refused == Refused::AllBusy {
                        tracing::debug!(path = %path.display(), "too many folder checks are stuck");
                        Checked::Busy
                    } else {
                        Checked::TimedOut
                    };
                }
                tokio::time::sleep(SLOT_RETRY.min(deadline - now)).await;
            }
        }
    };
    let answered = tokio::time::timeout_at(deadline, rx.wait_for(Option::is_some)).await;
    match answered {
        Ok(Ok(answer)) => answer
            .as_ref()
            .and_then(|a| a.downcast_ref::<T>())
            .cloned()
            .map_or(Checked::TimedOut, Checked::Answered),
        // Timed out, or the check panicked (its sender is gone). A stuck
        // check is counted by its mount: make sure that is the right one
        // (a share mounted since the list was read).
        _ => {
            refresh_mounts();
            Checked::TimedOut
        }
    }
}

/// The metadata of `path` (following links). The error, when there is one,
/// keeps its kind.
pub async fn metadata(
    path: &Path,
    timeout: Duration,
) -> Result<Result<std::fs::Metadata, std::io::ErrorKind>, NoAnswer> {
    let p = path.to_path_buf();
    guarded("metadata", path, timeout, move || {
        std::fs::metadata(&p).map_err(|e| e.kind())
    })
    .await
}

/// The metadata of `path` itself (not following a link).
pub async fn symlink_metadata(
    path: &Path,
    timeout: Duration,
) -> Result<Result<std::fs::Metadata, std::io::ErrorKind>, NoAnswer> {
    let p = path.to_path_buf();
    guarded("symlink_metadata", path, timeout, move || {
        std::fs::symlink_metadata(&p).map_err(|e| e.kind())
    })
    .await
}

/// Wait until one of `paths` stops answering, and return it. Each is looked
/// at (its metadata) now and then every `every`; one that gives no answer
/// within `timeout` hasn't answered. Any answer counts, a missing file
/// included. Never returns while they all answer: meant to run next to a
/// long step (`tokio::select!`), which is stopped when this returns.
pub async fn first_unanswered(paths: &[PathBuf], every: Duration, timeout: Duration) -> PathBuf {
    loop {
        for p in paths {
            let looked = checked("watch", p, timeout, {
                let p = p.clone();
                move || std::fs::metadata(&p).map(|_| ()).map_err(|e| e.kind())
            })
            .await;
            match looked {
                Checked::Answered(_) => {}
                Checked::TimedOut => return p.clone(),
                // Other shares are stuck; that says nothing about this one.
                Checked::Busy => {}
            }
        }
        tokio::time::sleep(every).await;
    }
}

/// The mount `path` is on, which its checks are counted by: the longest
/// mount point above it in `/proc/self/mountinfo` (read without touching
/// the share), else (no list of mounts) its first two folders.
pub async fn group_of(path: &Path) -> PathBuf {
    current_mounts().await;
    lock(&SLOTS).group(path).point
}

/// How much of a path stands for its mount when no mount point is above it
/// (counting the root): its first two folders. Tests, which make up their
/// mounts, keep each test's libraries apart (`/tmp/<test>/media/<library>`).
const FALLBACK_COMPONENTS: usize = if cfg!(any(test, feature = "test-hooks")) {
    5
} else {
    3
};

/// [`group_of`] with the mounts given (the later of two at one place, the
/// one on top), and the first `fallback` components of the path when none
/// is above it.
fn group_in(mounts: &[Mount], path: &Path, fallback: usize) -> Mount {
    mounts
        .iter()
        .filter(|m| path.starts_with(&m.point))
        .max_by_key(|m| m.point.components().count())
        .cloned()
        .unwrap_or_else(|| Mount {
            id: None,
            point: path.components().take(fallback).collect(),
        })
}

/// A mount point listed in `/proc/self/mountinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountPoint {
    /// Where it is mounted.
    pub path: PathBuf,
    /// Mounted on demand by an automounter (on an `autofs` mount): it comes
    /// and goes by itself, and a look at its folder mounts it again.
    pub on_demand: bool,
}

/// The mount points listed now, read afresh (without touching any share),
/// or `None` when the list can't be read.
pub async fn mount_points() -> Option<Vec<MountPoint>> {
    Some(points_of(fresh_mounts().await?))
}

/// The mount points of the mounts `listed`.
fn points_of(listed: Vec<Listed>) -> Vec<MountPoint> {
    let autofs: Vec<u64> = listed
        .iter()
        .filter(|l| l.fstype == "autofs")
        .filter_map(|l| l.mount.id)
        .collect();
    listed
        .into_iter()
        .map(|l| MountPoint {
            on_demand: l.parent.is_some_and(|p| autofs.contains(&p)),
            path: l.mount.point,
        })
        .collect()
}

/// The first of `points` (mount points a job's folders sit on) that isn't
/// mounted now: its folder is then an ordinary folder on the disk below
/// (an unmounted share leaves its mount point behind), where nothing may
/// be written. `None` when they all are, or the list can't be read.
pub async fn first_unmounted(points: &[PathBuf]) -> Option<PathBuf> {
    if points.is_empty() {
        return None;
    }
    let now = mount_points().await?;
    points
        .iter()
        .find(|p| !now.iter().any(|m| m.path == **p))
        .cloned()
}

/// One line of `/proc/self/mountinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Listed {
    mount: Mount,
    /// The id of the mount it is on.
    parent: Option<u64>,
    /// Its filesystem type.
    fstype: String,
}

/// The mounts checks are counted by, from the list.
#[cfg(any(test, not(feature = "test-hooks")))]
fn mounts_of(listed: &[Listed]) -> Vec<Mount> {
    listed.iter().map(|l| l.mount.clone()).collect()
}

/// Read the list of mounts when it hasn't been yet, and again in the
/// background once it is older than [`MOUNTS_MAX_AGE`] (this check goes by
/// the list it has).
#[cfg(not(any(test, feature = "test-hooks")))]
async fn current_mounts() {
    let read = lock(&SLOTS).mounts_read;
    match read {
        None => {
            let asked = Instant::now();
            let listed = tokio::task::spawn_blocking(read_mounts)
                .await
                .ok()
                .flatten();
            note_mounts(listed.as_deref(), asked);
        }
        Some(at) if at.elapsed() > MOUNTS_MAX_AGE => reread_mounts(),
        Some(_) => {}
    }
}

/// Count checks by the mounts `listed`, read at `asked` (`None`: the list
/// couldn't be read; the one there is stays, and isn't read again at
/// once).
#[cfg(not(any(test, feature = "test-hooks")))]
fn note_mounts(listed: Option<&[Listed]>, asked: Instant) {
    let mut slots = lock(&SLOTS);
    match listed {
        Some(listed) => slots.set_mounts(mounts_of(listed), asked),
        None => slots.mounts_read = Some(asked),
    }
}

/// The list of mounts, read now on a blocking thread (checks are counted
/// by it from then on too).
#[cfg(not(any(test, feature = "test-hooks")))]
async fn fresh_mounts() -> Option<Vec<Listed>> {
    let asked = Instant::now();
    let listed = tokio::task::spawn_blocking(read_mounts)
        .await
        .ok()
        .flatten()?;
    note_mounts(Some(&listed), asked);
    Some(listed)
}

/// Read the list of mounts again in the background, unless it was read
/// moments ago: a check that got no answer, or no room on its mount, may
/// be counted with the wrong mount (a share mounted since the list was
/// read), and its slot moves to the right one once the list is read.
#[cfg(not(any(test, feature = "test-hooks")))]
fn refresh_mounts() {
    let fresh = lock(&SLOTS)
        .mounts_read
        .is_some_and(|at| at.elapsed() < MOUNTS_MIN_AGE);
    if !fresh {
        reread_mounts();
    }
}

/// Read the list of mounts on a blocking thread (one read at a time), and
/// count checks by it.
#[cfg(not(any(test, feature = "test-hooks")))]
fn reread_mounts() {
    static READING: AtomicBool = AtomicBool::new(false);
    /// Lets the next read start when this one ends, however it ends.
    struct Reading;
    impl Drop for Reading {
        fn drop(&mut self) {
            READING.store(false, Ordering::SeqCst);
        }
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    if READING.swap(true, Ordering::SeqCst) {
        return;
    }
    let reading = Reading;
    runtime.spawn_blocking(move || {
        let _reading = reading;
        let asked = Instant::now();
        note_mounts(read_mounts().as_deref(), asked);
    });
}

/// Tests count checks by the mounts they make up (see [`hang::mount`]):
/// the machine's own would put every test's folders on one mount. Read
/// afresh by every check (they cost nothing to read).
#[cfg(any(test, feature = "test-hooks"))]
async fn current_mounts() {
    refresh_mounts();
}

#[cfg(any(test, feature = "test-hooks"))]
fn refresh_mounts() {
    let mounts = hang::mounts();
    lock(&SLOTS).set_mounts(mounts, Instant::now());
}

#[cfg(any(test, feature = "test-hooks"))]
async fn fresh_mounts() -> Option<Vec<Listed>> {
    let mounts = hang::mounts();
    lock(&SLOTS).set_mounts(mounts.clone(), Instant::now());
    Some(
        mounts
            .into_iter()
            .map(|mount| Listed {
                mount,
                parent: None,
                fstype: "test".to_string(),
            })
            .collect(),
    )
}

/// The mounts in `/proc/self/mountinfo` (`None` when it can't be read).
#[cfg(not(any(test, feature = "test-hooks")))]
fn read_mounts() -> Option<Vec<Listed>> {
    match std::fs::read("/proc/self/mountinfo") {
        Ok(text) => Some(parse_listed(&text)),
        Err(e) => {
            tracing::debug!("could not read the list of mounts: {e}");
            None
        }
    }
}

/// The mount points listed in `/proc/self/mountinfo`: the fifth field of
/// each line, with `\040`-style escapes for spaces and the like.
pub fn parse_mountinfo(text: &[u8]) -> Vec<PathBuf> {
    parse_listed(text)
        .into_iter()
        .map(|l| l.mount.point)
        .collect()
}

/// The lines of `/proc/self/mountinfo`: the mount's id is the first field,
/// the id of the mount it is on the second, the mount point the fifth, and
/// the filesystem type the field after the lone `-`.
fn parse_listed(text: &[u8]) -> Vec<Listed> {
    let number = |field: &[u8]| std::str::from_utf8(field).ok().and_then(|i| i.parse().ok());
    text.split(|b| *b == b'\n')
        .filter_map(|line| {
            let fields: Vec<&[u8]> = line.split(|b| *b == b' ').collect();
            let point = fields.get(4).filter(|f| !f.is_empty())?;
            let fstype = fields
                .iter()
                .skip(6)
                .skip_while(|f| **f != b"-".as_slice())
                .nth(1)
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .unwrap_or_default();
            Some(Listed {
                mount: Mount {
                    id: fields.first().and_then(|f| number(f)),
                    point: path_from_bytes(unescape(point)),
                },
                parent: fields.get(1).and_then(|f| number(f)),
                fstype,
            })
        })
        .collect()
}

fn unescape(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        if field[i] == b'\\'
            && let Some(octal) = field.get(i + 1..i + 4)
            && octal.iter().all(|b| (b'0'..=b'7').contains(b))
        {
            let value = octal
                .iter()
                .fold(0u32, |acc, b| acc * 8 + u32::from(b - b'0'));
            if let Ok(byte) = u8::try_from(value) {
                out.push(byte);
                i += 4;
                continue;
            }
        }
        out.push(field[i]);
        i += 1;
    }
    out
}

#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt as _;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
}

/// Tests stand in for a share that stopped answering: every [`guarded`]
/// check of a path under one marked hung blocks until the mark is lifted.
/// They also stand in for the mounts checks are counted by, and for checks
/// that can't start because too many are stuck elsewhere.
#[cfg(any(test, feature = "test-hooks"))]
pub mod hang {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{LazyLock, Mutex};
    use std::time::Duration;

    use super::{Mount, lock};

    /// Marked paths, each with the id of its mark.
    type Marks = Mutex<Vec<(PathBuf, u64)>>;

    static HUNG: LazyLock<Marks> = LazyLock::new(Mutex::default);
    static BUSY: LazyLock<Marks> = LazyLock::new(Mutex::default);
    static MOUNTS: LazyLock<Marks> = LazyLock::new(Mutex::default);
    static NEXT_MARK: AtomicU64 = AtomicU64::new(1);

    /// Make every check of `path`, and of anything under it, hang until
    /// the returned guard is dropped (even by a failing test, whose runtime
    /// would otherwise wait for the stuck thread forever).
    pub fn hang(path: &Path) -> Marked {
        mark(&HUNG, path)
    }

    /// Make every check of `path`, and of anything under it, find no room
    /// to start ([`super::NoAnswer::Busy`]) until the guard is dropped.
    pub fn busy(path: &Path) -> Marked {
        mark(&BUSY, path)
    }

    /// Mount something at `path`: checks of anything under it are counted
    /// as checks of one mount (tests otherwise count by a path's first
    /// folders), and it is listed as mounted ([`super::mount_points`]),
    /// until the guard is dropped (unmounted). Each mount is another one,
    /// also at a place where one was mounted before.
    pub fn mount(path: &Path) -> Marked {
        mark(&MOUNTS, path)
    }

    fn mark(list: &'static Marks, path: &Path) -> Marked {
        let id = NEXT_MARK.fetch_add(1, Ordering::SeqCst);
        lock(list).push((path.to_path_buf(), id));
        Marked { list, id }
    }

    /// Lifts its mark when dropped.
    #[derive(Debug)]
    pub struct Marked {
        list: &'static Marks,
        id: u64,
    }

    /// The guard of [`hang`].
    pub type Hung = Marked;

    impl Drop for Marked {
        fn drop(&mut self) {
            lock(self.list).retain(|(_, id)| *id != self.id);
        }
    }

    /// Whether `path` is (under) a path marked hung.
    pub fn is_hung(path: &Path) -> bool {
        lock(&HUNG).iter().any(|(p, _)| path.starts_with(p))
    }

    /// Whether `path` is (under) a path marked busy.
    pub fn is_busy(path: &Path) -> bool {
        lock(&BUSY).iter().any(|(p, _)| path.starts_with(p))
    }

    /// The mounts tests made up, in the order they were mounted.
    pub(super) fn mounts() -> Vec<Mount> {
        lock(&MOUNTS)
            .iter()
            .map(|(point, id)| Mount {
                id: Some(*id),
                point: point.clone(),
            })
            .collect()
    }

    /// Block while `path` is marked hung.
    pub fn wait_while_hung(path: &Path) {
        while is_hung(path) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Checks running now, in all.
    pub fn running_checks() -> usize {
        lock(&super::SLOTS).running.len()
    }

    /// Checks running now that are counted by the mount `path` is on.
    pub async fn running_checks_on(path: &Path) -> usize {
        super::current_mounts().await;
        let slots = lock(&super::SLOTS);
        slots.on(&slots.group(path)).len()
    }
}

#[cfg(not(any(test, feature = "test-hooks")))]
mod hang {
    use std::path::Path;

    #[inline]
    pub(super) fn wait_while_hung(_: &Path) {}

    #[inline]
    pub(super) fn is_busy(_: &Path) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A hung folder costs one thread, however often it is asked about.
    #[tokio::test]
    async fn a_hung_check_is_not_started_again_while_it_runs() {
        static STARTED: AtomicUsize = AtomicUsize::new(0);
        let release = Arc::new(std::sync::Barrier::new(2));
        let path = Path::new("/hung/share/for-tests");
        let mut asks = Vec::new();
        for _ in 0..20 {
            let release = Arc::clone(&release);
            asks.push(tokio::spawn(async move {
                guarded("test", path, Duration::from_millis(100), move || {
                    STARTED.fetch_add(1, Ordering::SeqCst);
                    release.wait();
                    7u32
                })
                .await
            }));
        }
        for ask in asks {
            assert_eq!(
                ask.await.unwrap(),
                Err(NoAnswer::NotAnswering),
                "not answered in time"
            );
        }
        assert_eq!(STARTED.load(Ordering::SeqCst), 1);

        // Once the folder answers, the next check runs and answers (the
        // first ask may still get the stuck check's late answer).
        release.wait();
        let mut answer = Err(NoAnswer::NotAnswering);
        for _ in 0..500 {
            answer = guarded("test", path, Duration::from_secs(5), || 8u32).await;
            if answer == Ok(8) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(answer, Ok(8));
        assert_eq!(STARTED.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn answers_come_back_with_their_error_kind() {
        let dir = tempfile::tempdir().unwrap();
        let m = metadata(dir.path(), Duration::from_secs(30)).await.unwrap();
        assert!(m.unwrap().is_dir());
        let missing = dir.path().join("missing");
        let m = metadata(&missing, Duration::from_secs(30)).await.unwrap();
        assert_eq!(m.unwrap_err(), std::io::ErrorKind::NotFound);
        let m = symlink_metadata(&missing, Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(m.unwrap_err(), std::io::ErrorKind::NotFound);
    }

    /// The watch over a long step returns the path that stopped answering,
    /// and nothing while they answer (a missing file is an answer).
    #[tokio::test]
    async fn the_watch_names_what_stopped_answering() {
        let dir = tempfile::tempdir().unwrap();
        let fine = dir.path().join("fine");
        let gone = dir.path().join("gone");
        let share = dir.path().join("share");
        std::fs::create_dir(&fine).unwrap();
        std::fs::create_dir(&share).unwrap();
        let paths = vec![fine.clone(), gone.clone(), share.join("film.mkv")];
        let quiet = tokio::time::timeout(
            Duration::from_millis(500),
            first_unanswered(&paths, Duration::from_millis(20), Duration::from_secs(30)),
        )
        .await;
        assert!(quiet.is_err(), "everything answers: {quiet:?}");

        let _hung = hang::hang(&share);
        let stuck = tokio::time::timeout(
            Duration::from_secs(60),
            first_unanswered(
                &paths,
                Duration::from_millis(20),
                Duration::from_millis(300),
            ),
        )
        .await
        .expect("the hung share is noticed");
        assert_eq!(stuck, share.join("film.mkv"));
    }

    /// The watch over a long step isn't stopped by checks that couldn't
    /// start (too many stuck elsewhere): that says nothing about its files.
    #[tokio::test]
    async fn the_watch_ignores_checks_that_could_not_start() {
        let dir = tempfile::tempdir().unwrap();
        let _busy = hang::busy(dir.path());
        let paths = vec![dir.path().join("film.mkv")];
        let quiet = tokio::time::timeout(
            Duration::from_millis(500),
            first_unanswered(&paths, Duration::from_millis(20), Duration::from_millis(50)),
        )
        .await;
        assert!(quiet.is_err(), "busy is not \"not answering\": {quiet:?}");
    }

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    /// Mounts at `points`, with ids.
    fn mounts(points: &[(&str, u64)]) -> Vec<Mount> {
        points
            .iter()
            .map(|(point, id)| Mount {
                id: Some(*id),
                point: PathBuf::from(point),
            })
            .collect()
    }

    /// Slots counted by mounts at `points` (numbered in order).
    fn slots_with_mounts(points: &[&str], now: Instant) -> Slots {
        let mut slots = Slots::default();
        let numbered: Vec<(&str, u64)> = points.iter().copied().zip(1..).collect();
        slots.set_mounts(mounts(&numbered), now);
        slots
    }

    /// Checks running that are counted by the mount `path` is on now.
    fn on_mount_of(slots: &Slots, path: &str) -> usize {
        slots.on(&slots.group(Path::new(path))).len()
    }

    /// One mount can hold only so many checks; once they are all stuck a
    /// further check of that mount is refused as not answering at once,
    /// while other mounts still have room.
    #[test]
    fn one_mount_cant_take_the_room_of_others() {
        let limits = Limits {
            per_group: 2,
            total: 5,
        };
        let now = Instant::now();
        let mut slots = slots_with_mounts(&["/mnt/hung", "/mnt/healthy"], now);
        let (hung, healthy) = (Path::new("/mnt/hung"), Path::new("/mnt/healthy"));
        let a = slots.take(hung, now, at(now, 100), limits).unwrap();
        let _b = slots.take(hung, now, at(now, 200), limits).unwrap();
        // Full, but one of them may still answer: wait (not refused yet).
        assert_eq!(
            slots.take(hung, at(now, 150), at(now, 300), limits),
            Err(Refused::GroupFull)
        );
        // Both past their callers' timeout: the mount isn't answering.
        assert!(!slots.is_stuck(hung, at(now, 150), limits));
        assert!(slots.is_stuck(hung, at(now, 250), limits));
        assert_eq!(
            slots.take(hung, at(now, 250), at(now, 300), limits),
            Err(Refused::GroupStuck)
        );
        // Another mount isn't affected.
        assert!(
            slots
                .take(healthy, at(now, 250), at(now, 300), limits)
                .is_ok()
        );
        // One ends: room again.
        slots.give_back(a);
        assert!(slots.take(hung, at(now, 250), at(now, 300), limits).is_ok());
        assert_eq!(slots.running.len(), 3);
    }

    /// With as many checks as may run in all running elsewhere, a check of
    /// a mount with room is refused as busy (unknown), not as not answering.
    #[test]
    fn a_full_pool_is_busy_not_unanswered() {
        let limits = Limits {
            per_group: 2,
            total: 4,
        };
        let now = Instant::now();
        let mut slots = slots_with_mounts(&["/a", "/b", "/healthy"], now);
        for share in ["/a", "/a", "/b", "/b"] {
            slots
                .take(Path::new(share), now, at(now, 10), limits)
                .unwrap();
        }
        assert_eq!(
            slots.take(Path::new("/healthy"), at(now, 50), at(now, 60), limits),
            Err(Refused::AllBusy)
        );
        // A stuck mount is still recognised as such.
        assert_eq!(
            slots.take(Path::new("/a"), at(now, 50), at(now, 60), limits),
            Err(Refused::GroupStuck)
        );
    }

    /// A share mounted after the list of mounts was read has its checks
    /// counted with the mount above it ("/"). Once they are stuck, only the
    /// share's folder is refused; the rest of "/" still has room. Once the
    /// list is read again, the stuck checks are counted by the share's own
    /// mount, and "/" is free again. Before, they stayed counted against
    /// "/" for as long as the share hung, and every check of a healthy
    /// library on "/" answered "not responding" at once.
    #[test]
    fn a_share_mounted_after_the_list_was_read_doesnt_stop_the_mount_above() {
        let limits = Limits {
            per_group: 3,
            total: 20,
        };
        let now = Instant::now();
        let mut slots = slots_with_mounts(&["/"], now);
        for folder in ["a", "b", "c"] {
            let path = Path::new("/mnt/new").join(folder);
            slots.take(&path, now, at(now, 100), limits).unwrap();
        }
        let later = at(now, 200);
        let healthy = Path::new("/data/Movies");
        assert!(!slots.is_stuck(healthy, later, limits));
        assert_eq!(
            slots.take(Path::new("/mnt/new/d"), later, at(now, 300), limits),
            Err(Refused::GroupStuck)
        );
        let h = slots.take(healthy, later, at(now, 300), limits).unwrap();
        slots.give_back(h);

        // The list is read again: the share is a mount of its own.
        slots.set_mounts(mounts(&[("/", 1), ("/mnt/new", 2)]), later);
        assert_eq!(on_mount_of(&slots, "/data"), 0);
        assert_eq!(on_mount_of(&slots, "/mnt/new"), 3);
        for i in 0..limits.per_group {
            let path = Path::new("/data").join(format!("folder {i}"));
            assert!(slots.take(&path, later, at(now, 300), limits).is_ok());
        }
        assert_eq!(
            slots.take(Path::new("/mnt/new/e"), later, at(now, 300), limits),
            Err(Refused::GroupStuck)
        );
    }

    /// While checks of one folder of a mount are slow but not given up on
    /// yet, a check elsewhere on that mount may take room beyond its share
    /// (up to twice as many); one of the slow folder waits.
    #[test]
    fn slow_checks_of_one_folder_leave_room_for_the_rest_of_the_mount() {
        let limits = Limits {
            per_group: 2,
            total: 20,
        };
        let now = Instant::now();
        let mut slots = slots_with_mounts(&["/"], now);
        let overdue = now + Duration::from_secs(60);
        slots
            .take(Path::new("/mnt/link/a"), now, overdue, limits)
            .unwrap();
        slots
            .take(Path::new("/mnt/link/b"), now, overdue, limits)
            .unwrap();
        // Not slow yet: wait.
        assert_eq!(
            slots.take(Path::new("/data/x"), at(now, 1), overdue, limits),
            Err(Refused::GroupFull)
        );
        let slow = now + SLOW_CHECK + Duration::from_millis(1);
        assert_eq!(
            slots.take(Path::new("/mnt/link/c"), slow, overdue, limits),
            Err(Refused::GroupFull)
        );
        assert!(
            slots
                .take(Path::new("/data/x"), slow, overdue, limits)
                .is_ok()
        );
        assert!(
            slots
                .take(Path::new("/data/y"), slow, overdue, limits)
                .is_ok()
        );
        // Twice its share: full.
        assert_eq!(
            slots.take(Path::new("/data/z"), slow, overdue, limits),
            Err(Refused::GroupFull)
        );
    }

    #[test]
    fn common_folders_are_found_component_by_component() {
        let paths = [
            Path::new("/mnt/nas/Movies/a.mkv"),
            Path::new("/mnt/nas/Movies2/b.mkv"),
        ];
        assert_eq!(common_folder(&paths), Path::new("/mnt/nas"));
        assert_eq!(
            common_folder(&[Path::new("/mnt/nas/a"), Path::new("/mnt/nas/a")]),
            Path::new("/mnt/nas/a")
        );
        assert_eq!(
            common_folder(&[Path::new("/a/b"), Path::new("/c")]),
            Path::new("/")
        );
    }

    /// The same through real checks: a share whose checks got stuck while
    /// they were counted with the mount above it (tests make the mounts up)
    /// leaves that mount's other folders answering, and once the share is
    /// known as a mount, its stuck checks are counted by it.
    #[tokio::test]
    async fn checks_stuck_on_a_new_share_leave_its_parent_mount_alone() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        let share = parent.join("share");
        let healthy = parent.join("healthy");
        std::fs::create_dir_all(&healthy).unwrap();
        let _parent = hang::mount(&parent);
        let _hung = hang::hang(&share);
        let timeout = Duration::from_millis(300);
        let mut asks = Vec::new();
        for i in 0..12 {
            let folder = share.join(format!("folder {i}"));
            asks.push(tokio::spawn(async move {
                guarded("browse", &folder, timeout, || ()).await
            }));
        }
        for ask in asks {
            assert_eq!(ask.await.unwrap(), Err(NoAnswer::NotAnswering));
        }
        assert!(hang::running_checks_on(&parent).await <= MAX_STUCK_PER_MOUNT);

        let listed = |path: PathBuf| async move {
            let p = path.clone();
            let asked = Instant::now();
            let listed = guarded("browse", &path, Duration::from_secs(30), move || {
                std::fs::read_dir(&p).is_ok()
            })
            .await;
            (listed, asked.elapsed())
        };
        let (answer, took) = listed(healthy.clone()).await;
        assert_eq!(answer, Ok(true), "the healthy folder answers");
        assert!(took < Duration::from_secs(10), "{took:?}");
        // The share itself is refused at once.
        let asked = Instant::now();
        let more = guarded(
            "browse",
            &share.join("one more"),
            Duration::from_secs(30),
            || (),
        )
        .await;
        assert_eq!(more, Err(NoAnswer::NotAnswering));
        assert!(asked.elapsed() < Duration::from_secs(10));

        // The share turns out to be a mount of its own: its stuck checks
        // are counted by it from then on.
        let _mounted = hang::mount(&share);
        assert_eq!(hang::running_checks_on(&share).await, MAX_STUCK_PER_MOUNT);
        let deadline = Instant::now() + Duration::from_secs(30);
        while hang::running_checks_on(&parent).await > 0 {
            assert!(
                Instant::now() < deadline,
                "the parent mount still counts checks"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let (answer, _) = listed(healthy).await;
        assert_eq!(answer, Ok(true));
    }

    /// Seventy different folders on one share that stopped answering cost at
    /// most [`MAX_STUCK_PER_MOUNT`] threads; the rest answer "not answering"
    /// at once, and a folder on another mount still answers.
    #[tokio::test]
    async fn a_hung_share_costs_a_few_threads_however_many_folders_are_looked_at() {
        let dir = tempfile::tempdir().unwrap();
        let share = dir.path().join("share");
        let healthy = dir.path().join("healthy");
        std::fs::create_dir(&healthy).unwrap();
        let _mount = hang::mount(&share);
        let _hung = hang::hang(&share);
        let timeout = Duration::from_millis(300);
        let mut asks = Vec::new();
        for i in 0..70 {
            let folder = share.join(format!("folder {i}"));
            asks.push(tokio::spawn(async move {
                guarded("browse", &folder, timeout, || ()).await
            }));
        }
        for ask in asks {
            assert_eq!(ask.await.unwrap(), Err(NoAnswer::NotAnswering));
        }
        assert!(hang::running_checks_on(&share).await <= MAX_STUCK_PER_MOUNT);

        // Now that its checks are all stuck, the share is refused at once,
        // also for a folder whose check is one of the stuck ones.
        for folder in ["one more", "folder 0", "folder 69"] {
            let asked = Instant::now();
            let more = guarded(
                "browse",
                &share.join(folder),
                Duration::from_secs(30),
                || (),
            )
            .await;
            assert_eq!(more, Err(NoAnswer::NotAnswering));
            assert!(
                asked.elapsed() < Duration::from_secs(10),
                "{:?}",
                asked.elapsed()
            );
        }

        // The healthy folder answers.
        let h = healthy.clone();
        let listed = guarded("browse", &healthy, Duration::from_secs(30), move || {
            std::fs::read_dir(&h).is_ok()
        })
        .await;
        assert_eq!(listed, Ok(true));
    }

    #[test]
    fn checks_are_counted_by_the_longest_mount_above_them() {
        let mounts = mounts(&[
            ("/", 1),
            ("/mnt/user", 2),
            ("/mnt/remotes/nas", 3),
            ("/mnt", 4),
            ("/mnt/remotes/nas", 5),
        ]);
        let group = |path: &str| group_in(&mounts, Path::new(path), 3);
        // The later of two at one place: the one on top.
        assert_eq!(
            group("/mnt/remotes/nas/Movies/a.mkv"),
            mounts[4],
            "{:?}",
            group("/mnt/remotes/nas/Movies/a.mkv")
        );
        assert_eq!(group("/mnt/user/TV").point, Path::new("/mnt/user"));
        // Component-wise: /mnt/username is not under /mnt/user.
        assert_eq!(group("/mnt/username/x").point, Path::new("/mnt"));
        assert_eq!(group("/config").point, Path::new("/"));
        // Without a list of mounts: the first two folders.
        assert_eq!(
            group_in(&[], Path::new("/media/share/Movies/a.mkv"), 3),
            Mount {
                id: None,
                point: PathBuf::from("/media/share")
            }
        );
    }

    #[test]
    fn mountinfo_lists_mount_points() {
        let text = b"22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
            40 22 0:35 / /mnt/remotes/My\\040Share rw - cifs //nas/share rw\n\
            41 22 0:36 / /media/caf\\303\\251 rw - fuse x rw\n";
        assert_eq!(
            parse_mountinfo(text),
            vec![
                PathBuf::from("/"),
                PathBuf::from("/mnt/remotes/My Share"),
                PathBuf::from("/media/café"),
            ]
        );
        assert_eq!(
            mounts_of(&parse_listed(text)),
            mounts(&[
                ("/", 22),
                ("/mnt/remotes/My Share", 40),
                ("/media/café", 41)
            ])
        );
        let types: Vec<(Option<u64>, String)> = parse_listed(text)
            .into_iter()
            .map(|l| (l.parent, l.fstype))
            .collect();
        assert_eq!(
            types,
            [
                (Some(1), "ext4".to_string()),
                (Some(22), "cifs".to_string()),
                (Some(22), "fuse".to_string()),
            ]
        );
    }

    /// A share an automounter mounted on demand (on an `autofs` mount) is
    /// told apart: it comes and goes by itself.
    #[test]
    fn shares_mounted_on_demand_are_told_apart() {
        let text = b"22 1 8:1 / / rw - ext4 /dev/sda1 rw\n\
            30 22 0:40 / /mnt/auto rw - autofs auto.nas rw\n\
            31 30 0:41 / /mnt/auto/nas rw - nfs4 nas:/export rw\n\
            32 22 0:42 / /mnt/nas rw - nfs4 nas:/other rw\n";
        let on_demand: Vec<(String, bool)> = points_of(parse_listed(text))
            .into_iter()
            .map(|m| (m.path.display().to_string(), m.on_demand))
            .collect();
        assert_eq!(
            on_demand,
            [
                ("/".to_string(), false),
                ("/mnt/auto".to_string(), false),
                ("/mnt/auto/nas".to_string(), true),
                ("/mnt/nas".to_string(), false),
            ]
        );
    }

    /// Checks stuck on two shares, one of them with checks in two of its
    /// folders (twice its room), stay counted by those shares once they
    /// are unmounted while they hang (`umount -l`): the mount above them
    /// keeps all its room. Before, they were counted by it from the next
    /// read of the list on, and with that many stuck there a healthy
    /// folder on it got no room for as long as the shares hung.
    #[test]
    fn checks_stuck_on_an_unmounted_share_are_charged_to_no_other_mount() {
        let limits = Limits {
            per_group: 2,
            total: 50,
        };
        let now = Instant::now();
        let mut slots = slots_with_mounts(&["/", "/mnt/a", "/mnt/b"], now);
        let overdue = at(now, 100);
        for path in ["/mnt/a/x/1", "/mnt/a/x/2", "/mnt/b/1", "/mnt/b/2"] {
            slots.take(Path::new(path), now, overdue, limits).unwrap();
        }
        // Slow and stuck in a/x; a/y is elsewhere on that share.
        let later = at(now, 1_000);
        for path in ["/mnt/a/y/1", "/mnt/a/y/2"] {
            slots
                .take(Path::new(path), later, at(now, 1_100), limits)
                .unwrap();
        }
        let stuck = at(now, 5_000);
        assert_eq!(on_mount_of(&slots, "/mnt/a"), 4);
        assert_eq!(
            slots.take(Path::new("/mnt/a/z"), stuck, at(now, 6_000), limits),
            Err(Refused::GroupStuck)
        );

        // Both shares are unmounted lazily: their checks hang on.
        slots.set_mounts(mounts(&[("/", 1)]), stuck);
        assert_eq!(on_mount_of(&slots, "/data"), 0);
        assert_eq!(slots.running.len(), 6);
        for i in 0..limits.per_group {
            let path = Path::new("/data/Movies").join(format!("{i}"));
            assert!(
                slots.take(&path, stuck, at(now, 6_000), limits).is_ok(),
                "{i}"
            );
        }
        // Their mount points are ordinary folders on "/" now, with room.
        slots.running.retain(|s| !s.path.starts_with("/data"));
        assert!(
            slots
                .take(Path::new("/mnt/a"), stuck, at(now, 6_000), limits)
                .is_ok()
        );
    }

    /// A share mounted again where one hangs is another mount: the checks
    /// stuck on the old one aren't counted by it, whether the list was
    /// read in between or not.
    #[test]
    fn a_share_mounted_again_at_the_same_place_is_another_mount() {
        let limits = Limits {
            per_group: 2,
            total: 50,
        };
        let now = Instant::now();
        for read_in_between in [false, true] {
            let mut slots = slots_with_mounts(&["/", "/mnt/nas"], now);
            for path in ["/mnt/nas/1", "/mnt/nas/2"] {
                slots
                    .take(Path::new(path), now, at(now, 100), limits)
                    .unwrap();
            }
            let later = at(now, 1_000);
            assert_eq!(
                slots.take(Path::new("/mnt/nas/3"), later, at(now, 2_000), limits),
                Err(Refused::GroupStuck)
            );
            if read_in_between {
                slots.set_mounts(mounts(&[("/", 1)]), later);
            }
            slots.set_mounts(mounts(&[("/", 1), ("/mnt/nas", 7)]), later);
            assert_eq!(on_mount_of(&slots, "/mnt/nas"), 0);
            assert!(
                slots
                    .take(Path::new("/mnt/nas/3"), later, at(now, 2_000), limits)
                    .is_ok()
            );
        }
    }

    /// However many checks are stuck on a share counted with a healthy
    /// mount (reached through a link, say), in however many of its
    /// folders, they never take the room of checks away from them; a
    /// check of the share itself is refused once its checks are stuck.
    #[test]
    fn stuck_checks_never_take_the_room_of_folders_away_from_them() {
        let limits = Limits {
            per_group: 2,
            total: 50,
        };
        let now = Instant::now();
        let mut slots = slots_with_mounts(&["/"], now);
        let mut t = 0;
        // Two folders of the share, then a second share: each fills the
        // room left beside the slow ones.
        for folder in ["/data/link/a", "/data/link/b", "/data/link2"] {
            for i in 0..limits.per_group {
                let path = Path::new(folder).join(format!("{i}"));
                slots
                    .take(&path, at(now, t), at(now, t + 100), limits)
                    .unwrap();
            }
            t += 1_000;
        }
        assert_eq!(on_mount_of(&slots, "/"), 6);
        let later = at(now, t);
        assert_eq!(
            slots.take(Path::new("/data/link/c"), later, at(now, t + 100), limits),
            Err(Refused::GroupStuck)
        );
        for i in 0..limits.per_group {
            let path = Path::new("/media/Movies").join(format!("{i}"));
            assert!(
                slots.take(&path, later, at(now, t + 100), limits).is_ok(),
                "{i}"
            );
        }
        // Its room is for checks that are not slow: full, until one ends.
        assert_eq!(
            slots.take(Path::new("/media/TV"), later, at(now, t + 100), limits),
            Err(Refused::GroupFull)
        );
    }

    /// The list of mounts is only taken from a fresh read, and only a
    /// mount that appeared since, between the one a check is counted by
    /// and its path, takes the check over.
    #[test]
    fn only_a_mount_that_appeared_below_takes_a_check_over() {
        let limits = Limits {
            per_group: 4,
            total: 50,
        };
        let now = Instant::now();
        let mut slots = slots_with_mounts(&["/", "/mnt/a"], now);
        slots
            .take(Path::new("/mnt/a/x/1"), now, at(now, 100), limits)
            .unwrap();
        slots
            .take(Path::new("/srv/y/1"), now, at(now, 100), limits)
            .unwrap();
        // /mnt/a/x mounted since: the check under it moves there. /srv
        // mounted since: so does the one under it (it was on "/").
        slots.set_mounts(
            mounts(&[("/", 1), ("/mnt/a", 2), ("/mnt/a/x", 3), ("/srv", 4)]),
            now,
        );
        assert_eq!(on_mount_of(&slots, "/mnt/a/x"), 1);
        assert_eq!(on_mount_of(&slots, "/srv"), 1);
        assert_eq!(on_mount_of(&slots, "/mnt/a"), 0);
        // /mnt/a unmounted, and the share at /mnt/a/x with it: the check
        // stays counted by /mnt/a/x, never by "/".
        slots.set_mounts(mounts(&[("/", 1), ("/srv", 4)]), now);
        assert_eq!(on_mount_of(&slots, "/data"), 0);
        // Mounted again (another mount, at one place or below): it stays.
        slots.set_mounts(
            mounts(&[("/", 1), ("/srv", 4), ("/mnt/a", 8), ("/mnt/a/x", 9)]),
            now,
        );
        assert_eq!(on_mount_of(&slots, "/mnt/a/x"), 0);
        assert_eq!(on_mount_of(&slots, "/mnt/a"), 0);
        assert_eq!(slots.running.len(), 2);
    }

    /// The same through real checks: two shares that hang, each with
    /// twice its room of stuck checks (in two of its folders), unmounted
    /// lazily while they hang. A healthy folder on the mount above still
    /// answers, quickly, and none of their checks are counted by it.
    #[tokio::test]
    async fn checks_stuck_on_unmounted_shares_leave_the_mount_above_alone() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("parent");
        let healthy = parent.join("healthy");
        std::fs::create_dir_all(&healthy).unwrap();
        let _parent = hang::mount(&parent);
        let shares = [parent.join("a"), parent.join("b")];
        let mut mounted = Vec::new();
        let mut hung = Vec::new();
        for share in &shares {
            mounted.push(hang::mount(share));
            hung.push(hang::hang(share));
        }
        let timeout = Duration::from_millis(300);
        for share in &shares {
            for folder in ["one", "two"] {
                let mut asks = Vec::new();
                for i in 0..12 {
                    let path = share.join(folder).join(format!("{i}"));
                    asks.push(tokio::spawn(async move {
                        guarded("browse", &path, timeout, || ()).await
                    }));
                }
                for ask in asks {
                    assert_eq!(ask.await.unwrap(), Err(NoAnswer::NotAnswering));
                }
            }
            let stuck = hang::running_checks_on(share).await;
            assert!(
                (MAX_STUCK_PER_MOUNT..=2 * MAX_STUCK_PER_MOUNT).contains(&stuck),
                "{stuck}"
            );
        }
        assert_eq!(hang::running_checks_on(&parent).await, 0);

        // Unmounted while they hang.
        mounted.clear();
        assert_eq!(hang::running_checks_on(&parent).await, 0);
        for i in 0..3 {
            let h = healthy.clone();
            let asked = Instant::now();
            let listed = guarded("browse", &healthy, Duration::from_secs(30), move || {
                std::fs::read_dir(&h).is_ok()
            })
            .await;
            assert_eq!(listed, Ok(true), "look {i}");
            assert!(
                asked.elapsed() < Duration::from_secs(10),
                "{:?}",
                asked.elapsed()
            );
        }
        drop(hung);
    }
}
