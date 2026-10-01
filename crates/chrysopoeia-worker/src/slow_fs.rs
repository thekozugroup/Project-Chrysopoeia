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
//!   answering costs at most that many threads, however many different
//!   folders on it are looked at. A check elsewhere on a full mount, away
//!   from where its checks are slow, may take room beyond that (up to twice
//!   as many): what is slow there is then another share counted with this
//!   mount (one mounted since the list of mounts was read, or reached
//!   through a link), and it must not stop the rest of the mount;
//! - the list of mounts is read again whenever a check gets no answer in
//!   time or finds no room on its mount (at most once a second), and the
//!   checks running are counted by the new list, so a share mounted since
//!   it was read soon has its stuck checks counted by its own mount;
//! - at most [`MAX_STUCK_CHECKS`] run at once in all, which leaves room for
//!   healthy folders while many shares hang. A check that can't start for
//!   that reason is [`NoAnswer::Busy`]: nothing is known about its folder,
//!   and it is never taken for one that stopped answering.
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
    /// The mount points checks are counted by (see [`group_in`]).
    mounts: Arc<Vec<PathBuf>>,
    /// When they were read (`None`: not yet).
    mounts_read: Option<Instant>,
}

/// One running check.
#[derive(Debug)]
struct Slot {
    id: u64,
    /// What it looks at.
    path: PathBuf,
    /// The mount it is counted by, by the mount points as last read.
    group: PathBuf,
    started: Instant,
    /// When the caller that started it gave up waiting for it: from then
    /// on it counts as stuck.
    overdue_at: Instant,
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
    fn group(&self, path: &Path) -> PathBuf {
        group_in(&self.mounts, path, FALLBACK_COMPONENTS)
    }

    /// The checks running on `group`.
    fn on(&self, group: &Path) -> Vec<&Slot> {
        self.running.iter().filter(|s| s.group == group).collect()
    }

    /// Room for a check of `path`, started now and given up on at
    /// `overdue_at`: its slot's id.
    ///
    /// A mount holds [`Limits::per_group`] checks. Once that many are stuck
    /// there, a check of anything in the folder they have in common is
    /// refused at once (`GroupStuck`). A check elsewhere on a full mount,
    /// away from the folder its slow checks (stuck, or running for
    /// [`SLOW_CHECK`]) have in common, may take room beyond that, up to
    /// twice as many: what is slow there is then most likely another share
    /// counted with this mount (one mounted since the list of mounts was
    /// read, or reached through a link), and must not stop the rest of
    /// the mount for as long as it hangs.
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
            let slow: Vec<&Path> = on_mount
                .iter()
                .filter(|s| s.started + SLOW_CHECK <= now || s.overdue_at <= now)
                .map(|s| s.path.as_path())
                .collect();
            let elsewhere =
                slow.len() >= limits.per_group && !path.starts_with(common_folder(&slow));
            if !elsewhere || on_mount.len() >= 2 * limits.per_group {
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

    /// Count checks by `mounts` from now on, those running included (a
    /// share mounted since the list was last read had its checks counted
    /// with the mount above it). `read_at`: when the list was read.
    fn set_mounts(&mut self, mounts: Vec<PathBuf>, read_at: Instant) {
        self.mounts_read = Some(read_at);
        if *self.mounts == mounts {
            return;
        }
        self.mounts = Arc::new(mounts);
        let mounts = Arc::clone(&self.mounts);
        for slot in &mut self.running {
            slot.group = group_in(&mounts, &slot.path, FALLBACK_COMPONENTS);
        }
    }
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
    lock(&SLOTS).group(path)
}

/// How much of a path stands for its mount when no mount point is above it
/// (counting the root): its first two folders. Tests, which make up their
/// mounts, keep each test's libraries apart (`/tmp/<test>/media/<library>`).
const FALLBACK_COMPONENTS: usize = if cfg!(any(test, feature = "test-hooks")) {
    5
} else {
    3
};

/// [`group_of`] with the mount points given, and the first `fallback`
/// components of the path when none is above it.
fn group_in(mounts: &[PathBuf], path: &Path, fallback: usize) -> PathBuf {
    mounts
        .iter()
        .filter(|m| path.starts_with(m))
        .max_by_key(|m| m.components().count())
        .cloned()
        .unwrap_or_else(|| path.components().take(fallback).collect())
}

/// Read the list of mount points when it hasn't been yet, and again in the
/// background once it is older than [`MOUNTS_MAX_AGE`] (this check goes by
/// the list it has).
#[cfg(not(any(test, feature = "test-hooks")))]
async fn current_mounts() {
    let read = lock(&SLOTS).mounts_read;
    match read {
        None => {
            let asked = Instant::now();
            let points = tokio::task::spawn_blocking(read_mount_points)
                .await
                .unwrap_or_default();
            lock(&SLOTS).set_mounts(points, asked);
        }
        Some(at) if at.elapsed() > MOUNTS_MAX_AGE => reread_mounts(),
        Some(_) => {}
    }
}

/// Read the list of mount points again in the background, unless it was
/// read moments ago: a check that got no answer, or no room on its mount,
/// may be counted with the wrong mount (a share mounted since the list was
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

/// Read the list of mount points on a blocking thread (one read at a time),
/// and count checks by it.
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
        let points = read_mount_points();
        lock(&SLOTS).set_mounts(points, asked);
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

/// The mount points in `/proc/self/mountinfo` (none when it can't be read).
#[cfg(not(any(test, feature = "test-hooks")))]
fn read_mount_points() -> Vec<PathBuf> {
    match std::fs::read("/proc/self/mountinfo") {
        Ok(text) => parse_mountinfo(&text),
        Err(e) => {
            tracing::debug!("could not read the list of mounts: {e}");
            Vec::new()
        }
    }
}

/// The mount points listed in `/proc/self/mountinfo`: the fifth field of
/// each line, with `\040`-style escapes for spaces and the like.
pub fn parse_mountinfo(text: &[u8]) -> Vec<PathBuf> {
    text.split(|b| *b == b'\n')
        .filter_map(|line| line.split(|b| *b == b' ').nth(4))
        .filter(|field| !field.is_empty())
        .map(|field| path_from_bytes(unescape(field)))
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
    use std::sync::{LazyLock, Mutex};
    use std::time::Duration;

    use super::lock;

    static HUNG: LazyLock<Mutex<Vec<PathBuf>>> = LazyLock::new(Mutex::default);
    static BUSY: LazyLock<Mutex<Vec<PathBuf>>> = LazyLock::new(Mutex::default);
    static MOUNTS: LazyLock<Mutex<Vec<PathBuf>>> = LazyLock::new(Mutex::default);

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

    /// Count checks of anything under `path` as checks of one mount (tests
    /// otherwise count by a path's first two folders) until the guard is
    /// dropped.
    pub fn mount(path: &Path) -> Marked {
        mark(&MOUNTS, path)
    }

    fn mark(list: &'static Mutex<Vec<PathBuf>>, path: &Path) -> Marked {
        lock(list).push(path.to_path_buf());
        Marked {
            list,
            path: path.to_path_buf(),
        }
    }

    /// Lifts its mark when dropped.
    #[derive(Debug)]
    pub struct Marked {
        list: &'static Mutex<Vec<PathBuf>>,
        path: PathBuf,
    }

    /// The guard of [`hang`].
    pub type Hung = Marked;

    impl Drop for Marked {
        fn drop(&mut self) {
            let mut list = lock(self.list);
            if let Some(i) = list.iter().position(|p| *p == self.path) {
                list.remove(i);
            }
        }
    }

    /// Whether `path` is (under) a path marked hung.
    pub fn is_hung(path: &Path) -> bool {
        lock(&HUNG).iter().any(|p| path.starts_with(p))
    }

    /// Whether `path` is (under) a path marked busy.
    pub fn is_busy(path: &Path) -> bool {
        lock(&BUSY).iter().any(|p| path.starts_with(p))
    }

    /// The mounts tests made up.
    pub(super) fn mounts() -> Vec<PathBuf> {
        lock(&MOUNTS).clone()
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

    /// Checks running now on the mount `path` is counted by.
    pub async fn running_checks_on(path: &Path) -> usize {
        let group = super::group_of(path).await;
        lock(&super::SLOTS).on(&group).len()
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

    fn slots_with_mounts(mounts: &[&str], now: Instant) -> Slots {
        let mut slots = Slots::default();
        slots.set_mounts(mounts.iter().map(PathBuf::from).collect(), now);
        slots
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
        slots.set_mounts(vec![PathBuf::from("/"), PathBuf::from("/mnt/new")], later);
        assert_eq!(slots.on(Path::new("/")).len(), 0);
        assert_eq!(slots.on(Path::new("/mnt/new")).len(), 3);
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
        let mounts: Vec<PathBuf> = ["/", "/mnt/user", "/mnt/remotes/nas", "/mnt"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(
            group_in(&mounts, Path::new("/mnt/remotes/nas/Movies/a.mkv"), 3),
            Path::new("/mnt/remotes/nas")
        );
        assert_eq!(
            group_in(&mounts, Path::new("/mnt/user/TV"), 3),
            Path::new("/mnt/user")
        );
        // Component-wise: /mnt/username is not under /mnt/user.
        assert_eq!(
            group_in(&mounts, Path::new("/mnt/username/x"), 3),
            Path::new("/mnt")
        );
        assert_eq!(group_in(&mounts, Path::new("/config"), 3), Path::new("/"));
        // Without a list of mounts: the first two folders.
        assert_eq!(
            group_in(&[], Path::new("/media/share/Movies/a.mkv"), 3),
            Path::new("/media/share")
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
    }
}
