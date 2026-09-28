//! Debounced folder watching.
//!
//! Each watched root gets its own kernel watcher (inotify on Linux) with one
//! watch per folder, added by walking the root the same way
//! [`crate::walk_library`] does. notify delivers raw events on its own
//! thread; its callback only forwards them over a channel. A tokio task, the
//! debouncer, coalesces bursts, pairs renames, waits for copies to finish and
//! emits [`WatchEvent`]s. Every filesystem check the debouncer makes runs on
//! the blocking pool, in at most one batch per root at a time, so the runtime
//! never blocks and a hung network share only delays its own library.
//!
//! # Which folders are watched
//!
//! - Folders a scan skips are not watched either: folders matched by the
//!   ignore patterns (recycle bins, `.Trash-*`, `@eaDir`, ...), DVD and
//!   Blu-ray disc copies (see [`crate::walk`]), folders whose names are not
//!   valid UTF-8, and folders reached through a symbolic link unless
//!   [`ScanOptions::follow_links`] is set. This keeps the kernel's watch
//!   budget for folders that matter.
//! - A folder that cannot be watched (for example one the container user may
//!   not read) is skipped with a warning naming it; the rest of the library
//!   is still watched. Only reaching the system limit on watched folders
//!   (`fs.inotify.max_user_watches`) makes [`LibraryWatcher::watch`] fail.
//! - Folders that appear later (created, copied or moved in) are watched as
//!   soon as the debouncer sees them, and the media already inside them is
//!   reported.
//!
//! # When a file counts as settled
//!
//! A changed media file is reported as [`WatchEvent::Upserted`] once no event
//! has arrived for it for the settle period, the same size and modification
//! time have been seen at least a settle period apart, and it can be opened
//! for reading. While a copy is running the file is checked at most about
//! once per settle period, so thousands of files arriving at once stay cheap.
//! Events are filtered exactly as a walk filters files: ignore patterns,
//! minimum size, disc copies, links and Chrysopoeia's own files.
//!
//! # Removals
//!
//! Removals are reported immediately, for files and folders alike, with
//! duplicates from the same burst dropped. Two exceptions keep the library
//! safe:
//!
//! - Chrysopoeia replaces a file by renaming the original to one of its
//!   backup names first. A rename *to* a backup name is therefore not
//!   reported as a removal; the new file appears under the original name
//!   moments later and is reported as [`WatchEvent::Upserted`].
//! - The deletion, move or unmount of a watched root itself is never
//!   reported (an unmounted disk must not empty a library).
//!
//! # Roots that go away and come back
//!
//! Every root is checked about every 30 seconds. When its folder is gone, its
//! kernel watches are dropped; when the folder is back, is now a different
//! folder, or its disk was unmounted and mounted again in the meantime (on
//! Linux a separate inotify instance hears the kernel drop the watches, which
//! notify does not report), it is watched afresh without any call from the
//! caller. Changes made while it was away are found by the next scan.
//! [`LibraryWatcher::watch`] on a root that is already watched runs the same
//! check right away. A root that cannot be watched again is retried every
//! five minutes.
//!
//! Limits: network shares (SMB/NFS) and some container setups deliver no
//! change events for changes made elsewhere; periodic rescans cover those.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use anyhow::anyhow;
use chrysopoeia_core::Settings;
use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind, RemoveKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::runtime::{Handle, RuntimeFlavor};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::MissedTickBehavior;
use walkdir::WalkDir;

use crate::ScanOptions;
use crate::walk::{
    IgnoreRules, SIDECAR_EXTENSIONS, disc_folder_note, disc_structure_note, has_extension_in,
    is_artifact_path, is_media,
};

/// Capacity of the channel returned by [`LibraryWatcher::start`].
const EVENT_BUFFER: usize = 4096;

/// How long the source of a rename waits for its destination before it
/// counts as moved out of the watched folders.
const RENAME_PAIR_WINDOW: Duration = Duration::from_millis(500);

/// Bounds of the debouncer's tick (a quarter of the settle period).
const MIN_TICK: Duration = Duration::from_millis(50);
const MAX_TICK: Duration = Duration::from_secs(1);

/// Minimum time to keep waiting for a file that cannot be checked or opened.
const MIN_GIVE_UP: Duration = Duration::from_secs(600);

/// How often every root is checked for having gone away or changed.
const ROOT_CHECK_INTERVAL: Duration = if cfg!(test) {
    Duration::from_millis(500)
} else {
    Duration::from_secs(30)
};

/// A batch of checks running longer than this is reported as stuck (at
/// least; four settle periods when that is longer).
const MIN_STUCK_AFTER: Duration = Duration::from_secs(30);

/// How long to wait before trying again, by itself, to watch a root that
/// could not be watched (for example because the system limit on watched
/// folders was reached: each try walks the whole library).
const REARM_BACKOFF: Duration = Duration::from_secs(300);

/// Longest wait for a quick look at a path from the debouncer task itself.
const QUICK_STAT_TIMEOUT: Duration = Duration::from_secs(2);

/// How many unwatchable folders are named one by one in the log.
const NAMED_PROBLEMS: usize = 5;

/// Most distinct warnings remembered to avoid repeating them.
const MAX_REMEMBERED_WARNINGS: usize = 256;

/// A settled change under a watched root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// A media file was created or modified and its size has stopped
    /// changing for the settle period (i.e. the copy finished).
    Upserted(PathBuf),
    /// A media file (or a folder containing media) was removed or moved away.
    Removed(PathBuf),
}

/// How events under one root are filtered, and which folders are watched.
#[derive(Debug)]
struct RootFilter {
    /// The patterns `rules` was compiled from, to tell whether new options
    /// change which folders are watched.
    patterns: Vec<String>,
    rules: IgnoreRules,
    min_size_bytes: u64,
    follow_links: bool,
}

impl RootFilter {
    fn new(opts: &ScanOptions) -> Self {
        Self {
            patterns: opts.ignore_patterns.clone(),
            rules: IgnoreRules::new(&opts.ignore_patterns),
            min_size_bytes: opts.min_size_bytes,
            follow_links: opts.follow_links,
        }
    }

    /// The filter of a root watched without options: the default settings.
    fn defaults() -> Self {
        Self::new(&ScanOptions::from_settings(&Settings::default()))
    }

    /// Whether `other` watches the same folders as this filter.
    fn same_folders(&self, other: &Self) -> bool {
        self.patterns == other.patterns && self.follow_links == other.follow_links
    }

    /// Whether the folder at `relative` (and everything in it) is skipped,
    /// by a pattern or because it is (inside) a disc copy.
    fn excludes_folder(&self, relative: &Path) -> bool {
        self.rules.excludes_folder(relative) || disc_structure_note(relative).is_some()
    }

    /// Whether a media file at `relative` is skipped.
    fn excludes_file(&self, relative: &Path) -> bool {
        self.rules.excludes_file(relative) || disc_structure_note(relative).is_some()
    }
}

/// Which filesystem object a folder is (device and inode on Unix), to notice
/// a root that was replaced, for example by unmounting and mounting a disk.
type Identity = Option<(u64, u64)>;

fn identity_of(metadata: &std::fs::Metadata) -> Identity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

/// The kernel watches of one root.
struct Armed {
    /// The root folder these watches were set up on.
    identity: Identity,
    watcher: Mutex<RecommendedWatcher>,
}

/// A watched root.
#[derive(Clone)]
struct RootEntry {
    filter: Arc<RootFilter>,
    /// Absent while the root folder is gone or cannot be watched.
    armed: Option<Arc<Armed>>,
    /// When the periodic check may next try to watch a root that could not
    /// be watched.
    retry_at: Option<Instant>,
}

/// Raw events from the kernel watchers.
type RawEvent = notify::Result<Event>;

/// State shared by the handle, the debouncer and the blocking pool.
struct Shared {
    /// Where every kernel watcher sends its raw events.
    raw_tx: mpsc::UnboundedSender<RawEvent>,
    /// Watched roots. Only locked for short map operations, never while
    /// touching the disk.
    roots: Mutex<HashMap<PathBuf, RootEntry>>,
    /// Serializes setting up and removing a root's watches. Only locked on
    /// blocking threads.
    arming: Mutex<()>,
    /// The last problem logged for each root by the periodic check, so it is
    /// not repeated every time.
    root_problems: Mutex<HashMap<PathBuf, String>>,
    /// Tells when the kernel dropped a root's watches (Linux only).
    canary: Option<Mutex<canary::Canary>>,
}

/// Lock a mutex, carrying on if a thread panicked while holding it (the
/// guarded data stays consistent: every critical section is a single map
/// operation or a single call into notify).
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Watches library roots. Dropping it stops watching.
pub struct LibraryWatcher {
    shared: Arc<Shared>,
    task: JoinHandle<()>,
    waiting: WaitingFiles,
}

/// How many media files the watcher is waiting on per watched root: files
/// with recent changes that haven't settled yet, typically copies in
/// progress. Updated about once per second (a quarter of the settle
/// period, when shorter). Cheap to clone; it reads empty once the watcher
/// has stopped.
#[derive(Debug, Clone, Default)]
pub struct WaitingFiles(Arc<Mutex<HashMap<PathBuf, usize>>>);

impl WaitingFiles {
    /// Files waited on, by root (roots without any are left out).
    pub fn counts(&self) -> HashMap<PathBuf, usize> {
        lock(&self.0).clone()
    }

    fn set(&self, counts: HashMap<PathBuf, usize>) {
        *lock(&self.0) = counts;
    }
}

impl fmt::Debug for LibraryWatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut roots: Vec<PathBuf> = lock(&self.shared.roots).keys().cloned().collect();
        roots.sort();
        f.debug_struct("LibraryWatcher")
            .field("roots", &roots)
            .finish_non_exhaustive()
    }
}

impl LibraryWatcher {
    /// Start a watcher. `settle` is how long a file's size must stay
    /// unchanged before `Upserted` is emitted. Temp/backup artifacts and
    /// non-media files never produce events.
    ///
    /// Must be called from inside a Tokio runtime (the debouncer is a task).
    pub fn start(settle: Duration) -> anyhow::Result<(Self, mpsc::Receiver<WatchEvent>)> {
        let runtime = Handle::try_current().map_err(|_| {
            anyhow!("The folder watcher must be started from inside the async runtime.")
        })?;
        let (raw_tx, raw_rx) = mpsc::unbounded_channel();
        // Fail now, with a clear message, when the platform cannot watch
        // folders at all (for example when the inotify instance limit is
        // reached).
        drop(
            kernel_watcher(raw_tx.clone())
                .map_err(|error| anyhow!(describe_notify_error(&error, None)))?,
        );

        let shared = Arc::new(Shared {
            raw_tx,
            roots: Mutex::new(HashMap::new()),
            arming: Mutex::new(()),
            root_problems: Mutex::new(HashMap::new()),
            canary: canary::Canary::new().map(Mutex::new),
        });
        let (out_tx, out_rx) = mpsc::channel(EVENT_BUFFER);
        let waiting = WaitingFiles::default();
        let debouncer = Debouncer::new(settle, Arc::clone(&shared), out_tx, waiting.clone());
        let task = runtime.spawn(debouncer.run(raw_rx));
        Ok((
            Self {
                shared,
                task,
                waiting,
            },
            out_rx,
        ))
    }

    /// Start watching a root recursively. Watching the same root twice is a no-op.
    ///
    /// Events are filtered like a walk with the default settings
    /// (`Settings::default()`): hidden files and folders, NAS recycle bins
    /// and thumbnail folders are ignored, as are Chrysopoeia's own files,
    /// non-media files, disc copies and links. Use
    /// [`LibraryWatcher::watch_with_options`] to apply the user's own
    /// patterns and minimum size; a root already watched with options keeps
    /// them. A relative `root` is taken relative to the current directory.
    ///
    /// Calling this for a root that is already watched checks that its
    /// folder is still the same one and watches it afresh when it is not (a
    /// disk that was unmounted and mounted again); the watcher also does this
    /// by itself every 30 seconds.
    ///
    /// Blocking: it walks the root to watch each of its folders. On a
    /// multi-threaded runtime other tasks move off the calling thread in the
    /// meantime; on a current-thread runtime call it from `spawn_blocking`.
    pub fn watch(&self, root: &Path) -> anyhow::Result<()> {
        self.add_root(root, None)
    }

    /// Like [`LibraryWatcher::watch`], but events are also filtered with the
    /// ignore patterns, minimum size and link setting in `opts`, exactly as
    /// [`crate::walk_library`] would filter them. Calling it for a root that
    /// is already watched updates the filter (and which folders are watched,
    /// when the patterns changed).
    pub fn watch_with_options(&self, root: &Path, opts: &ScanOptions) -> anyhow::Result<()> {
        self.add_root(root, Some(RootFilter::new(opts)))
    }

    /// The media files this watcher is waiting on because they are still
    /// being written, per root (see [`WaitingFiles`]).
    pub fn waiting_files(&self) -> WaitingFiles {
        self.waiting.clone()
    }

    fn add_root(&self, root: &Path, filter: Option<RootFilter>) -> anyhow::Result<()> {
        let root = absolute_root(root)?;
        let shared = &self.shared;
        run_blocking(|| shared.add_root(root, filter))
    }

    /// Stop watching a root. Unknown roots are a no-op.
    pub fn unwatch(&self, root: &Path) -> anyhow::Result<()> {
        let Ok(root) = std::path::absolute(root) else {
            return Ok(());
        };
        let shared = &self.shared;
        let removed = run_blocking(|| {
            let _arming = lock(&shared.arming);
            lock(&shared.roots).remove(&root)
        });
        if removed.is_some() {
            lock(&shared.root_problems).remove(&root);
            shared.canary_unwatch(&root);
            tracing::debug!(root = %root.display(), "stopped watching folder");
        }
        // Dropping the entry closes its kernel watcher.
        drop(removed);
        Ok(())
    }
}

impl Drop for LibraryWatcher {
    fn drop(&mut self) {
        self.task.abort();
        self.waiting.set(HashMap::new());
        let roots = std::mem::take(&mut *lock(&self.shared.roots));
        drop(roots);
    }
}

/// `root` as an absolute path: kernel watchers report absolute paths, so a
/// relative root would never match its events.
fn absolute_root(root: &Path) -> anyhow::Result<PathBuf> {
    std::path::absolute(root).map_err(|error| {
        anyhow!(
            "The folder \"{}\" cannot be watched for changes ({error}).",
            root.display()
        )
    })
}

/// A kernel watcher that forwards its events to the debouncer.
fn kernel_watcher(raw_tx: mpsc::UnboundedSender<RawEvent>) -> notify::Result<RecommendedWatcher> {
    notify::recommended_watcher(move |result: RawEvent| {
        if result.as_ref().is_ok_and(is_noise) {
            return;
        }
        // Fails only once the debouncer is gone, i.e. while shutting down.
        let _ = raw_tx.send(result);
    })
}

impl Shared {
    /// Implementation of [`LibraryWatcher::watch_with_options`] (blocking).
    fn add_root(&self, root: PathBuf, filter: Option<RootFilter>) -> anyhow::Result<()> {
        let _arming = lock(&self.arming);
        let existing = lock(&self.roots).get(&root).cloned();
        let filter = match (filter, &existing) {
            (Some(filter), _) => Arc::new(filter),
            (None, Some(entry)) => Arc::clone(&entry.filter),
            (None, None) => Arc::new(RootFilter::defaults()),
        };
        if let Some(RootEntry {
            filter: old_filter,
            armed: Some(armed),
            ..
        }) = &existing
        {
            let unchanged = std::fs::metadata(&root).is_ok_and(|metadata| {
                metadata.is_dir() && identity_of(&metadata) == armed.identity
            }) && !self.watches_died(&root);
            if unchanged && old_filter.same_folders(&filter) {
                if let Some(entry) = lock(&self.roots).get_mut(&root) {
                    entry.filter = filter;
                }
                return Ok(());
            }
        }
        match arm(&self.raw_tx, &root, &filter) {
            Ok(armed) => {
                let entry = RootEntry {
                    filter,
                    armed: Some(Arc::new(armed)),
                    retry_at: None,
                };
                let replaced = lock(&self.roots).insert(root.clone(), entry);
                drop(replaced);
                lock(&self.root_problems).remove(&root);
                self.canary_watch(&root);
                tracing::debug!(root = %root.display(), "watching folder for changes");
                Ok(())
            }
            Err(error) => {
                if existing.is_some() {
                    // Stay registered so the periodic check picks the root up
                    // again once it can be watched.
                    let entry = RootEntry {
                        filter,
                        armed: None,
                        retry_at: Some(Instant::now() + REARM_BACKOFF),
                    };
                    let replaced = lock(&self.roots).insert(root.clone(), entry);
                    drop(replaced);
                    self.canary_unwatch(&root);
                }
                Err(error)
            }
        }
    }

    /// The periodic check of one root (blocking): drop the watches of a root
    /// that went away, and watch a root afresh when it came back or is now a
    /// different folder.
    fn check_root(&self, root: &Path) {
        let Some(entry) = lock(&self.roots).get(root).cloned() else {
            return;
        };
        let died = self.watches_died(root);
        let current = std::fs::metadata(root)
            .ok()
            .filter(|metadata| metadata.is_dir())
            .map(|metadata| identity_of(&metadata));
        match (&entry.armed, current) {
            (Some(armed), Some(identity)) if armed.identity == identity && !died => {}
            (None, None) => {}
            (Some(armed), None) => {
                let dropped = {
                    let mut roots = lock(&self.roots);
                    match roots.get_mut(root) {
                        Some(entry)
                            if entry.armed.as_ref().is_some_and(|a| Arc::ptr_eq(a, armed)) =>
                        {
                            entry.armed.take()
                        }
                        _ => None,
                    }
                };
                if dropped.is_some() {
                    self.canary_unwatch(root);
                    self.log_root_problem(
                        root,
                        format!(
                            "The library folder {} was deleted, moved or unmounted, so it is not \
                             watched for now. Its files stay in the library, and watching resumes \
                             by itself when the folder is back.",
                            root.display()
                        ),
                    );
                }
            }
            (None, Some(_)) if entry.retry_at.is_some_and(|at| Instant::now() < at) => {}
            (_, Some(_)) => self.rearm(root, died),
        }
    }

    /// Watch a registered root afresh (blocking) when its folder changed, or
    /// in any case when `died` (the kernel dropped its watches).
    fn rearm(&self, root: &Path, died: bool) {
        let _arming = lock(&self.arming);
        // Re-read under the arming lock: the root may have been unwatched or
        // re-armed in the meantime.
        let Some(entry) = lock(&self.roots).get(root).cloned() else {
            return;
        };
        let current = std::fs::metadata(root)
            .ok()
            .filter(|metadata| metadata.is_dir())
            .map(|metadata| identity_of(&metadata));
        let stale = match (&entry.armed, current) {
            (_, None) => false,
            (Some(armed), Some(identity)) => died || armed.identity != identity,
            (None, Some(_)) => true,
        };
        if !stale {
            return;
        }
        match arm(&self.raw_tx, root, &entry.filter) {
            Ok(armed) => {
                let replaced = lock(&self.roots).get_mut(root).and_then(|slot| {
                    slot.retry_at = None;
                    slot.armed.replace(Arc::new(armed))
                });
                drop(replaced);
                lock(&self.root_problems).remove(root);
                self.canary_watch(root);
                tracing::info!(
                    root = %root.display(),
                    "The library folder is available again (or was mounted again), so it is \
                     watched afresh. Changes made in the meantime are found by the next scan."
                );
            }
            Err(error) => {
                // The old watches are no use any more; try again later.
                let dropped = lock(&self.roots).get_mut(root).and_then(|slot| {
                    slot.retry_at = Some(Instant::now() + REARM_BACKOFF);
                    slot.armed.take()
                });
                drop(dropped);
                self.canary_unwatch(root);
                self.log_root_problem(root, format!("{error:#}"));
            }
        }
    }

    /// Start telling when the kernel drops the watches of `root`.
    fn canary_watch(&self, root: &Path) {
        if let Some(canary) = &self.canary {
            lock(canary).watch(root);
        }
    }

    fn canary_unwatch(&self, root: &Path) {
        if let Some(canary) = &self.canary {
            lock(canary).unwatch(root);
        }
    }

    /// Whether the kernel dropped the watches of `root` since the last time
    /// this was asked (for example because its disk was unmounted, even if
    /// it was mounted again right away).
    fn watches_died(&self, root: &Path) -> bool {
        self.canary
            .as_ref()
            .is_some_and(|canary| lock(canary).died(root))
    }

    /// Log a warning about a root unless it is the same as the last one.
    fn log_root_problem(&self, root: &Path, message: String) {
        let mut problems = lock(&self.root_problems);
        if problems.get(root) != Some(&message) {
            tracing::warn!(root = %root.display(), "{message}");
            problems.insert(root.to_path_buf(), message);
        }
    }

    /// The registered roots and their state.
    fn snapshot(&self) -> Vec<(PathBuf, RootEntry)> {
        lock(&self.roots)
            .iter()
            .map(|(root, entry)| (root.clone(), entry.clone()))
            .collect()
    }
}

/// Noticing that the kernel dropped a root's watches.
///
/// When a filesystem is unmounted, inotify sends `IN_UNMOUNT` and
/// `IN_IGNORED` for every watch on it, which notify does not pass on. The
/// root may then be mounted again with the same device and inode numbers, so
/// comparing the folder's identity cannot tell either. A separate inotify
/// instance with a single watch per root receives those events instead.
#[cfg(target_os = "linux")]
mod canary {
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};

    use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask};

    /// One inotify instance watching every root folder itself.
    pub(super) struct Canary {
        inotify: Inotify,
        watches: HashMap<WatchDescriptor, Vec<PathBuf>>,
        /// Roots whose watches died and that were not asked about yet.
        dead: HashSet<PathBuf>,
    }

    impl Canary {
        /// `None` when no inotify instance is available; roots are then only
        /// compared by identity.
        pub(super) fn new() -> Option<Self> {
            match Inotify::init() {
                Ok(inotify) => Some(Self {
                    inotify,
                    watches: HashMap::new(),
                    dead: HashSet::new(),
                }),
                Err(error) => {
                    tracing::debug!(%error, "cannot tell when library disks are unmounted");
                    None
                }
            }
        }

        /// Start (or restart) telling when the watches of `root` die.
        pub(super) fn watch(&mut self, root: &Path) {
            self.drain();
            self.forget(root);
            self.dead.remove(root);
            match self
                .inotify
                .watches()
                .add(root, WatchMask::DELETE_SELF | WatchMask::MOVE_SELF)
            {
                Ok(wd) => {
                    let roots = self.watches.entry(wd).or_default();
                    if !roots.iter().any(|known| known == root) {
                        roots.push(root.to_path_buf());
                    }
                }
                Err(error) => tracing::debug!(
                    root = %root.display(),
                    %error,
                    "cannot tell when this library's disk is unmounted"
                ),
            }
        }

        /// Stop tracking `root`.
        pub(super) fn unwatch(&mut self, root: &Path) {
            if let Some(wd) = self.forget(root) {
                // Fails harmlessly when the kernel already dropped it.
                let _ = self.inotify.watches().remove(wd);
            }
            self.dead.remove(root);
        }

        /// Whether the watch on `root` died since the last call.
        pub(super) fn died(&mut self, root: &Path) -> bool {
            self.drain();
            self.dead.remove(root)
        }

        /// Stop tracking `root`; its watch descriptor when no other root
        /// shares it.
        fn forget(&mut self, root: &Path) -> Option<WatchDescriptor> {
            let wd = self
                .watches
                .iter()
                .find(|(_, roots)| roots.iter().any(|known| known == root))
                .map(|(wd, _)| wd.clone())?;
            let roots = self.watches.get_mut(&wd)?;
            roots.retain(|known| known != root);
            if roots.is_empty() {
                self.watches.remove(&wd);
                Some(wd)
            } else {
                None
            }
        }

        /// Read what the kernel reported. Never blocks: the instance is
        /// non-blocking, so an empty queue ends the loop.
        fn drain(&mut self) {
            let mut buffer = [0u8; 4096];
            loop {
                let Ok(events) = self.inotify.read_events(&mut buffer) else {
                    break;
                };
                let mut read_any = false;
                for event in events {
                    read_any = true;
                    if event.mask.contains(EventMask::Q_OVERFLOW) {
                        // Something was missed: assume the worst for all.
                        let all: Vec<PathBuf> = self.watches.values().flatten().cloned().collect();
                        self.dead.extend(all);
                    }
                    if event.mask.contains(EventMask::IGNORED)
                        && let Some(roots) = self.watches.remove(&event.wd)
                    {
                        self.dead.extend(roots);
                    }
                }
                if !read_any {
                    break;
                }
            }
        }
    }
}

/// Elsewhere roots are only compared by identity.
#[cfg(not(target_os = "linux"))]
mod canary {
    use std::path::Path;

    /// Never created on this platform.
    pub(super) struct Canary;

    impl Canary {
        pub(super) fn new() -> Option<Self> {
            None
        }

        pub(super) fn watch(&mut self, _root: &Path) {}

        pub(super) fn unwatch(&mut self, _root: &Path) {}

        pub(super) fn died(&mut self, _root: &Path) -> bool {
            false
        }
    }
}

/// Set up the kernel watches of a root (blocking): one watcher, one watch per
/// folder a walk would look into.
fn arm(
    raw_tx: &mpsc::UnboundedSender<RawEvent>,
    root: &Path,
    filter: &RootFilter,
) -> anyhow::Result<Armed> {
    let metadata = check_watch_root(root)?;
    let mut watcher = kernel_watcher(raw_tx.clone())
        .map_err(|e| anyhow!(describe_notify_error(&e, Some(root))))?;
    watcher
        .watch(root, RecursiveMode::NonRecursive)
        .map_err(|error| anyhow!(describe_notify_error(&error, Some(root))))?;
    let walked = walk_folders(root, root, filter, false, false, &mut |folder| {
        watcher.watch(folder, RecursiveMode::NonRecursive)
    });
    if let Some(error) = walked.limit {
        // Dropping the watcher releases the watches added so far.
        return Err(anyhow!(describe_notify_error(&error, Some(root))));
    }
    for message in walked.problem_messages() {
        tracing::warn!(root = %root.display(), "{message}");
    }
    Ok(Armed {
        identity: identity_of(&metadata),
        watcher: Mutex::new(watcher),
    })
}

/// What [`walk_folders`] found.
#[derive(Debug, Default)]
struct FolderWalk {
    /// Media files, when asked for.
    media: Vec<(PathBuf, Observed)>,
    /// Folders that could not be watched or read, with the reason.
    unwatchable: Vec<(PathBuf, String)>,
    /// The system limit on watched folders was reached; no more watches were
    /// added.
    limit: Option<notify::Error>,
}

impl FolderWalk {
    /// Plain-language warnings about the folders that are not watched.
    fn problem_messages(&self) -> Vec<String> {
        let mut messages: Vec<String> = self
            .unwatchable
            .iter()
            .take(NAMED_PROBLEMS)
            .map(|(folder, reason)| {
                format!(
                    "Chrysopoeia cannot watch the folder {} for changes ({reason}). Changes inside \
                     it are found by the regular rescans; the rest of the library is still \
                     watched.",
                    folder.display()
                )
            })
            .collect();
        if self.unwatchable.len() > NAMED_PROBLEMS {
            messages.push(format!(
                "{} more folders cannot be watched for changes either; they are covered by the \
                 regular rescans.",
                self.unwatchable.len() - NAMED_PROBLEMS
            ));
        }
        messages
    }
}

/// Walk `start` (the root, or a folder below it) and call `add_watch` for
/// every folder a scan would look into: ignored folders, disc copies, folders
/// with non-UTF-8 names and (unless links are followed) linked folders are
/// skipped. `watch_start` also watches `start` itself; `collect_media` lists
/// the media files found on the way (blocking).
fn walk_folders(
    root: &Path,
    start: &Path,
    filter: &RootFilter,
    watch_start: bool,
    collect_media: bool,
    add_watch: &mut dyn FnMut(&Path) -> notify::Result<()>,
) -> FolderWalk {
    let mut found = FolderWalk::default();
    let relative = |path: &Path| path.strip_prefix(root).ok().map(Path::to_path_buf);
    if start != root
        && (start.to_str().is_none()
            || relative(start).is_none_or(|rel| filter.excludes_folder(&rel))
            || (!filter.follow_links && through_link(root, start)))
    {
        return found;
    }
    let mut entries = WalkDir::new(start)
        .follow_links(filter.follow_links)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_dir()
                || (entry.path().to_str().is_some()
                    && disc_folder_note(entry.file_name()).is_none()
                    && !relative(entry.path()).is_some_and(|rel| filter.rules.is_ignored_dir(&rel)))
        });
    while let Some(entry) = entries.next() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                if let Some(path) = error.path().filter(|_| error.depth() > 0) {
                    let reason = if error.loop_ancestor().is_some() {
                        "it links back to a folder above it".to_string()
                    } else {
                        error
                            .io_error()
                            .map_or_else(|| error.to_string(), describe_io_error)
                    };
                    found.unwatchable.push((path.to_path_buf(), reason));
                }
                continue;
            }
        };
        let path = entry.path();
        if entry.file_type().is_dir() {
            if (entry.depth() == 0 && !watch_start) || found.limit.is_some() {
                continue;
            }
            if let Err(error) = add_watch(path) {
                if is_limit_error(&error) {
                    found.limit = Some(error);
                    if !collect_media {
                        break;
                    }
                } else {
                    found
                        .unwatchable
                        .push((path.to_path_buf(), describe_watch_error(&error)));
                    entries.skip_current_dir();
                }
            }
            continue;
        }
        if !collect_media || !entry.file_type().is_file() || path.to_str().is_none() {
            continue;
        }
        if !is_media(path) || relative(path).is_some_and(|rel| filter.rules.is_ignored(&rel)) {
            continue;
        }
        if let Ok(metadata) = entry.metadata() {
            let observed = Observed {
                size: metadata.len(),
                modified: metadata.modified().ok(),
            };
            found.media.push((path.to_path_buf(), observed));
        }
    }
    found
}

/// Whether a folder between `root` and `path` (exclusive) is a symbolic
/// link (blocking).
fn through_link(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    relative
        .ancestors()
        .skip(1)
        .take_while(|dir| !dir.as_os_str().is_empty())
        .any(|dir| {
            std::fs::symlink_metadata(root.join(dir)).is_ok_and(|m| m.file_type().is_symlink())
        })
}

/// Run a blocking call from sync code that may be on a runtime thread,
/// telling a multi-threaded runtime to move its other tasks elsewhere.
fn run_blocking<R>(f: impl FnOnce() -> R) -> R {
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

/// Check that `root` is a folder that can be watched (blocking).
fn check_watch_root(root: &Path) -> anyhow::Result<std::fs::Metadata> {
    let shown = root.display();
    match std::fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => Ok(metadata),
        Ok(_) => Err(anyhow!(
            "{shown} is a file, not a folder, so it cannot be watched for changes."
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(anyhow!(
            "The folder {shown} does not exist, so it cannot be watched for changes."
        )),
        Err(error) => Err(anyhow!(
            "The folder {shown} cannot be watched for changes ({error})."
        )),
    }
}

/// Events the debouncer would drop anyway, filtered on notify's thread so a
/// busy disk does not flood the channel: reads (media servers scanning,
/// ffprobe) and writes to files that are not media (downloads in progress
/// under a `.part` name, artwork, Chrysopoeia's own temporary files).
fn is_noise(event: &Event) -> bool {
    match event.kind {
        EventKind::Create(CreateKind::File)
        | EventKind::Modify(
            ModifyKind::Data(_) | ModifyKind::Metadata(_) | ModifyKind::Any | ModifyKind::Other,
        )
        | EventKind::Access(AccessKind::Close(AccessMode::Write)) => {
            !event.paths.iter().any(|path| is_media(path))
        }
        EventKind::Access(_) => true,
        _ => false,
    }
}

/// Whether a notify error means the system limit on watched folders was
/// reached.
fn is_limit_error(error: &notify::Error) -> bool {
    match &error.kind {
        notify::ErrorKind::MaxFilesWatch => true,
        notify::ErrorKind::Io(io_error) => is_errno(io_error, ENOSPC),
        _ => false,
    }
}

/// Why one folder could not be watched, in a few words.
fn describe_watch_error(error: &notify::Error) -> String {
    match &error.kind {
        notify::ErrorKind::Io(io_error) => describe_io_error(io_error),
        notify::ErrorKind::PathNotFound => "it no longer exists".to_string(),
        _ => error.to_string(),
    }
}

fn describe_io_error(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::PermissionDenied => {
            "Chrysopoeia does not have permission to read it".to_string()
        }
        io::ErrorKind::NotFound => "it no longer exists".to_string(),
        _ => error.to_string(),
    }
}

/// Explain a notify error in plain language, with the fix when there is one.
///
/// The folder named is the one that failed, except for the system-wide limit
/// on watched folders, where the library root says more.
fn describe_notify_error(error: &notify::Error, root: Option<&Path>) -> String {
    let failed = error.paths.first().map(PathBuf::as_path);
    let folder = if is_limit_error(error) {
        root.or(failed)
    } else {
        failed.or(root)
    }
    .map_or_else(
        || "the library folders".to_string(),
        |p| p.display().to_string(),
    );
    match &error.kind {
        _ if is_limit_error(error) => watch_limit_message(&folder),
        notify::ErrorKind::Io(io_error) if is_errno(io_error, EMFILE) => format!(
            "Cannot watch {folder} for changes: the system limit on folder watchers \
             (fs.inotify.max_user_instances) has been reached. Raise it on the host, for example \
             with `sysctl -w fs.inotify.max_user_instances=512`. Periodic rescans still find new \
             and changed files."
        ),
        notify::ErrorKind::Io(io_error) if io_error.kind() == io::ErrorKind::PermissionDenied => {
            format!(
                "Chrysopoeia does not have permission to watch {folder} for changes. Check the \
                 folder's owner and permissions (PUID/PGID in Docker). Periodic rescans still find \
                 new and changed files."
            )
        }
        notify::ErrorKind::PathNotFound => {
            format!("The folder {folder} does not exist, so it cannot be watched for changes.")
        }
        _ => format!(
            "Cannot watch {folder} for changes ({error}). Periodic rescans still find new and \
             changed files."
        ),
    }
}

fn watch_limit_message(folder: &str) -> String {
    format!(
        "Cannot watch {folder} for changes: the system limit on watched folders \
         (fs.inotify.max_user_watches) has been reached. Raise it on the host, for example with \
         `sysctl -w fs.inotify.max_user_watches=524288` (on Unraid, the Tips and Tweaks plugin \
         has a setting for it). Periodic rescans still find new and changed files."
    )
}

/// Linux error numbers for the inotify limits.
const ENOSPC: i32 = 28;
const EMFILE: i32 = 24;

fn is_errno(error: &io::Error, errno: i32) -> bool {
    cfg!(target_os = "linux") && error.raw_os_error() == Some(errno)
}

/// Size and modification time of a file at one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Observed {
    size: u64,
    modified: Option<SystemTime>,
}

/// A path with recent activity, waiting to settle.
#[derive(Debug)]
struct Pending {
    last_event: Instant,
    observed: Option<Observed>,
    /// When `observed` was first seen.
    stable_since: Instant,
    next_check: Instant,
    /// Since when the file could not be opened or checked.
    trouble_since: Option<Instant>,
    /// The latest event could also have meant "gone" (a rename without a
    /// partner, or a platform that does not say what happened).
    uncertain: bool,
}

/// What the debouncer should do with a pending path after a check.
enum Verdict {
    Wait,
    Ready,
    GiveUp,
}

impl Pending {
    fn new(now: Instant, uncertain: bool) -> Self {
        Self {
            last_event: now,
            observed: None,
            stable_since: now,
            next_check: now,
            trouble_since: None,
            uncertain,
        }
    }

    /// A file found inside a folder that appeared.
    fn found(now: Instant, observed: Observed, settle: Duration) -> Self {
        Self {
            observed: Some(observed),
            next_check: now + settle,
            ..Self::new(now, false)
        }
    }

    fn settled(&self, now: Instant, settle: Duration) -> bool {
        self.observed.is_some()
            && now >= self.stable_since + settle
            && now >= self.last_event + settle
    }

    fn after_check(
        &mut self,
        observed: Observed,
        readable: Option<bool>,
        now: Instant,
        settle: Duration,
    ) -> Verdict {
        self.uncertain = false;
        if self.observed != Some(observed) {
            self.observed = Some(observed);
            self.stable_since = now;
            self.next_check = now + settle;
            self.trouble_since = None;
            return Verdict::Wait;
        }
        match readable {
            Some(true) => return Verdict::Ready,
            Some(false) => {
                if self.in_trouble_too_long(now, settle) {
                    return Verdict::GiveUp;
                }
            }
            None => {}
        }
        let due = self.stable_since.max(self.last_event) + settle;
        self.next_check = if due > now { due } else { now + settle };
        Verdict::Wait
    }

    fn after_failed_check(&mut self, now: Instant, settle: Duration) -> Verdict {
        if self.in_trouble_too_long(now, settle) {
            return Verdict::GiveUp;
        }
        self.next_check = now + settle;
        Verdict::Wait
    }

    fn in_trouble_too_long(&mut self, now: Instant, settle: Duration) -> bool {
        let since = *self.trouble_since.get_or_insert(now);
        now.duration_since(since) >= (settle * 20).max(MIN_GIVE_UP)
    }
}

/// How an event described a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Noted {
    /// A file was written: only media names matter.
    File,
    /// A folder was created: its contents are looked at.
    Folder,
    /// Could be a file, a folder, or gone: a check decides.
    Uncertain,
}

/// One path to check on the blocking pool.
struct Check {
    path: PathBuf,
    previous: Option<Observed>,
    open_if_unchanged: bool,
}

/// What a check found.
enum Finding {
    /// Not there. `in_existing_folder` tells a file that is really gone from
    /// one whose whole folder or disk went away (reported, or deliberately
    /// not reported, by other means).
    Missing {
        in_existing_folder: bool,
    },
    NotAFile,
    /// A folder: now watched, with the media files inside it.
    Folder(FolderWalk),
    File {
        observed: Observed,
        /// Whether it could be opened, when that was tried.
        readable: Option<bool>,
    },
    Failed(String),
}

/// Work the debouncer runs on the blocking pool.
enum Job {
    /// A batch of checks for one root.
    Checks(PathBuf),
    /// The periodic check of a root.
    RootCheck(PathBuf),
}

/// What a finished job produced.
enum Done {
    Checked(Vec<(PathBuf, Finding)>),
    RootChecked,
}

/// A batch of checks still running.
struct Busy {
    since: Instant,
    warned: bool,
}

/// The receiver of watch events was dropped.
struct Closed;

struct Debouncer {
    settle: Duration,
    shared: Arc<Shared>,
    out: mpsc::Sender<WatchEvent>,
    pending: HashMap<PathBuf, Pending>,
    /// Rename sources waiting for their destination, by rename cookie.
    renames: HashMap<usize, (PathBuf, Instant)>,
    /// Recently reported removals, to drop duplicates within a burst.
    removed: HashMap<PathBuf, Instant>,
    /// Jobs running on the blocking pool.
    work: JoinSet<Done>,
    jobs: HashMap<tokio::task::Id, Job>,
    /// Roots with a batch of checks running (at most one each, so a hung
    /// share holds up only its own library).
    busy: HashMap<PathBuf, Busy>,
    /// Roots whose periodic check is running.
    root_checks: HashSet<PathBuf>,
    next_root_check: Instant,
    /// Warnings already logged, so a stream of events does not repeat them.
    warned: HashSet<String>,
    /// Media files waited on per root, as last published.
    waiting: WaitingFiles,
    published: HashMap<PathBuf, usize>,
}

impl Debouncer {
    fn new(
        settle: Duration,
        shared: Arc<Shared>,
        out: mpsc::Sender<WatchEvent>,
        waiting: WaitingFiles,
    ) -> Self {
        Self {
            settle,
            shared,
            out,
            pending: HashMap::new(),
            renames: HashMap::new(),
            removed: HashMap::new(),
            work: JoinSet::new(),
            jobs: HashMap::new(),
            busy: HashMap::new(),
            root_checks: HashSet::new(),
            next_root_check: Instant::now() + ROOT_CHECK_INTERVAL,
            warned: HashSet::new(),
            waiting,
            published: HashMap::new(),
        }
    }

    async fn run(mut self, mut raw: mpsc::UnboundedReceiver<RawEvent>) {
        let mut ticker = tokio::time::interval((self.settle / 4).clamp(MIN_TICK, MAX_TICK));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let out = self.out.clone();
        loop {
            let step = tokio::select! {
                received = raw.recv() => match received {
                    Some(Ok(event)) => self.on_event(event).await,
                    Some(Err(error)) => {
                        self.warn_once(describe_notify_error(&error, None));
                        Ok(())
                    }
                    None => break,
                },
                Some(done) = self.work.join_next_with_id(), if !self.work.is_empty() => {
                    self.on_done(done).await
                }
                _ = ticker.tick() => self.on_tick().await,
                () = out.closed() => break,
            };
            if step.is_err() {
                break;
            }
        }
        self.waiting.set(HashMap::new());
        tracing::debug!("folder watcher stopped");
    }

    async fn on_event(&mut self, event: Event) -> Result<(), Closed> {
        let now = Instant::now();
        if event.need_rescan() {
            tracing::warn!(
                "Too many folder changes happened at once and some were missed; the next \
                 scheduled rescan will pick them up."
            );
        }
        let tracker = event.tracker();
        match event.kind {
            EventKind::Create(CreateKind::Folder) => self.note_all(event.paths, Noted::Folder, now),
            EventKind::Create(CreateKind::File)
            | EventKind::Modify(
                ModifyKind::Data(_) | ModifyKind::Metadata(_) | ModifyKind::Any | ModifyKind::Other,
            )
            | EventKind::Access(AccessKind::Close(AccessMode::Write)) => {
                self.note_all(event.paths, Noted::File, now);
            }
            EventKind::Modify(ModifyKind::Name(mode)) => {
                return self.on_rename(mode, tracker, event.paths, now).await;
            }
            EventKind::Remove(kind) => {
                let is_dir = match kind {
                    RemoveKind::Folder => Some(true),
                    RemoveKind::File => Some(false),
                    RemoveKind::Any | RemoveKind::Other => None,
                };
                for path in event.paths {
                    self.gone(path, is_dir).await?;
                }
            }
            EventKind::Access(_) => {}
            EventKind::Create(CreateKind::Any | CreateKind::Other)
            | EventKind::Any
            | EventKind::Other => self.note_all(event.paths, Noted::Uncertain, now),
        }
        Ok(())
    }

    async fn on_rename(
        &mut self,
        mode: RenameMode,
        tracker: Option<usize>,
        paths: Vec<PathBuf>,
        now: Instant,
    ) -> Result<(), Closed> {
        match mode {
            RenameMode::From => {
                for path in paths {
                    match tracker {
                        Some(id) => {
                            self.renames.insert(id, (path, now));
                        }
                        // A watched folder moved (inotify's MOVE_SELF), or a
                        // platform without rename cookies.
                        None => self.gone(path, None).await?,
                    }
                }
            }
            RenameMode::To => {
                for path in paths {
                    match tracker.and_then(|id| self.renames.remove(&id)) {
                        Some((from, _)) => self.renamed(from, path, now).await?,
                        None => self.note(path, Noted::Uncertain, now),
                    }
                }
            }
            RenameMode::Both => {
                // With a tracker (inotify) both halves were already handled
                // through their own From and To events.
                if tracker.is_none() {
                    let mut paths = paths.into_iter();
                    if let (Some(from), Some(to)) = (paths.next(), paths.next()) {
                        self.renamed(from, to, now).await?;
                    }
                }
            }
            RenameMode::Any | RenameMode::Other => self.note_all(paths, Noted::Uncertain, now),
        }
        Ok(())
    }

    async fn renamed(&mut self, from: PathBuf, to: PathBuf, now: Instant) -> Result<(), Closed> {
        if is_artifact_path(&to) && !is_artifact_path(&from) {
            // Chrysopoeia is moving an original aside while replacing it.
            self.pending.remove(&from);
        } else {
            // Bounded: a hung share must not stall the debouncer.
            let to_is_dir =
                tokio::time::timeout(QUICK_STAT_TIMEOUT, tokio::fs::symlink_metadata(&to))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .map(|metadata| metadata.is_dir());
            self.gone(from, to_is_dir).await?;
        }
        self.note(to, Noted::Uncertain, now);
        Ok(())
    }

    fn note_all(&mut self, paths: Vec<PathBuf>, noted: Noted, now: Instant) {
        for path in paths {
            self.note(path, noted, now);
        }
    }

    /// Remember activity on a path.
    fn note(&mut self, path: PathBuf, noted: Noted, now: Instant) {
        // Fast path for the stream of write events during a long copy: the
        // path already passed the checks below.
        if let Some(pending) = self.pending.get_mut(&path) {
            pending.last_event = now;
            pending.uncertain = noted == Noted::Uncertain;
            return;
        }
        if is_artifact_path(&path) || (noted == Noted::File && !is_media(&path)) {
            return;
        }
        let Some((root, entry)) = self.root_of(&path) else {
            return;
        };
        if path == root {
            return;
        }
        let filter = &entry.filter;
        let excluded = path.strip_prefix(&root).is_ok_and(|relative| match noted {
            Noted::File => filter.excludes_file(relative),
            Noted::Folder => filter.excludes_folder(relative),
            // Excluded either way; otherwise a check tells what it is.
            Noted::Uncertain => filter.excludes_file(relative) && filter.excludes_folder(relative),
        });
        if excluded {
            return;
        }
        self.removed.remove(&path);
        self.pending
            .insert(path, Pending::new(now, noted == Noted::Uncertain));
    }

    /// A path disappeared (`is_dir` says whether it was a folder, when known).
    async fn gone(&mut self, path: PathBuf, is_dir: Option<bool>) -> Result<(), Closed> {
        if is_dir == Some(false) {
            self.pending.remove(&path);
        } else {
            self.forget_below(&path);
        }
        if is_artifact_path(&path) {
            return Ok(());
        }
        let Some((root, _)) = self.root_of(&path) else {
            return Ok(());
        };
        if path == root {
            // Never reported: an unmounted disk must not empty a library.
            // The root's own check decides whether it is really gone (the
            // event may be a leftover from before it was watched afresh).
            self.check_root_soon(root);
            return Ok(());
        }
        let report = match is_dir {
            Some(true) => true,
            Some(false) => is_media(&path),
            // Unknown: report unless it was clearly a sidecar file.
            None => is_media(&path) || !has_extension_in(&path, SIDECAR_EXTENSIONS),
        };
        if report && !self.removed.contains_key(&path) {
            self.removed.insert(path.clone(), Instant::now());
            self.emit(WatchEvent::Removed(path)).await?;
        }
        Ok(())
    }

    /// Drop pending work for `path` and everything below it.
    fn forget_below(&mut self, path: &Path) {
        if !self.pending.is_empty() {
            self.pending.retain(|pending, _| !pending.starts_with(path));
        }
    }

    /// Start the periodic check of `root` now, unless it is already running.
    fn check_root_soon(&mut self, root: PathBuf) {
        if !self.root_checks.insert(root.clone()) {
            return;
        }
        let shared = Arc::clone(&self.shared);
        let job_root = root.clone();
        let handle = self.work.spawn_blocking(move || {
            if catch_unwind(AssertUnwindSafe(|| shared.check_root(&job_root))).is_err() {
                tracing::warn!(root = %job_root.display(), "checking a library folder failed");
            }
            Done::RootChecked
        });
        self.jobs.insert(handle.id(), Job::RootCheck(root));
    }

    async fn on_tick(&mut self) -> Result<(), Closed> {
        let now = Instant::now();
        let settle = self.settle;

        // A rename whose destination never appeared moved the path out of
        // the watched folders.
        let expired: Vec<usize> = self
            .renames
            .iter()
            .filter(|(_, (_, at))| now.duration_since(*at) >= RENAME_PAIR_WINDOW)
            .map(|(id, _)| *id)
            .collect();
        let mut moved_out: Vec<PathBuf> = expired
            .into_iter()
            .filter_map(|id| self.renames.remove(&id))
            .map(|(path, _)| path)
            .collect();
        moved_out.sort();
        for path in moved_out {
            self.gone(path, None).await?;
        }
        let keep_removed_for = settle.max(RENAME_PAIR_WINDOW);
        self.removed
            .retain(|_, at| now.duration_since(*at) < keep_removed_for);

        let roots = self.shared.snapshot();
        if now >= self.next_root_check {
            self.next_root_check = now + ROOT_CHECK_INTERVAL;
            for (root, _) in &roots {
                self.check_root_soon(root.clone());
            }
        }

        // Drop work for roots that are no longer watched.
        self.pending
            .retain(|path, _| roots.iter().any(|(root, _)| path.starts_with(root)));
        self.publish_waiting(&roots);

        // Group the due checks by root, leaving out roots whose previous
        // batch is still running.
        let mut batches: HashMap<PathBuf, Vec<Check>> = HashMap::new();
        for (path, pending) in &self.pending {
            if pending.next_check > now {
                continue;
            }
            let Some((root, _)) = innermost_root(&roots, path) else {
                continue;
            };
            if self.busy.contains_key(root) {
                continue;
            }
            batches.entry(root.clone()).or_default().push(Check {
                path: path.clone(),
                previous: pending.observed,
                open_if_unchanged: pending.settled(now, settle),
            });
        }
        for (root, checks) in batches {
            let Some((_, entry)) = roots.iter().find(|(r, _)| *r == root) else {
                continue;
            };
            let entry = entry.clone();
            let job_root = root.clone();
            let handle = self.work.spawn_blocking(move || {
                let findings = checks
                    .into_iter()
                    .map(|check| {
                        let path = check.path.clone();
                        catch_unwind(AssertUnwindSafe(|| inspect(check, &job_root, &entry)))
                            .unwrap_or_else(|_| {
                                (path, Finding::Failed("the check failed".to_string()))
                            })
                    })
                    .collect();
                Done::Checked(findings)
            });
            self.jobs.insert(handle.id(), Job::Checks(root.clone()));
            self.busy.insert(
                root,
                Busy {
                    since: now,
                    warned: false,
                },
            );
        }

        // Say so when a library's disk or share stops answering.
        let stuck_after = (settle * 4).max(MIN_STUCK_AFTER);
        for (root, busy) in &mut self.busy {
            if !busy.warned && now.duration_since(busy.since) >= stuck_after {
                busy.warned = true;
                tracing::warn!(
                    root = %root.display(),
                    "Checking changed files in this library is taking unusually long; its disk or \
                     network share may not be responding. Other libraries are still watched."
                );
            }
        }
        Ok(())
    }

    /// A job on the blocking pool finished.
    async fn on_done(
        &mut self,
        done: Result<(tokio::task::Id, Done), tokio::task::JoinError>,
    ) -> Result<(), Closed> {
        let (id, done) = match done {
            Ok((id, done)) => (id, Some(done)),
            Err(error) => {
                tracing::debug!(%error, "a folder watcher job failed");
                (error.id(), None)
            }
        };
        match self.jobs.remove(&id) {
            Some(Job::Checks(root)) => {
                if self.busy.remove(&root).is_some_and(|busy| busy.warned) {
                    tracing::info!(root = %root.display(), "The library's disk or share answers again.");
                }
            }
            Some(Job::RootCheck(root)) => {
                self.root_checks.remove(&root);
            }
            None => {}
        }
        match done {
            Some(Done::Checked(findings)) => self.apply(findings).await,
            Some(Done::RootChecked) | None => Ok(()),
        }
    }

    async fn apply(&mut self, findings: Vec<(PathBuf, Finding)>) -> Result<(), Closed> {
        let now = Instant::now();
        let settle = self.settle;
        let mut ready = Vec::new();
        for (path, finding) in findings {
            let verdict = match finding {
                Finding::Missing { in_existing_folder } => {
                    let was_uncertain = self.pending.remove(&path).is_some_and(|p| p.uncertain);
                    if was_uncertain && in_existing_folder {
                        self.gone(path, None).await?;
                    }
                    continue;
                }
                Finding::NotAFile => Verdict::GiveUp,
                Finding::Folder(walked) => {
                    self.pending.remove(&path);
                    if let Some(error) = &walked.limit {
                        self.warn_once(describe_notify_error(error, None));
                    }
                    for message in walked.problem_messages() {
                        self.warn_once(message);
                    }
                    for (file, observed) in walked.media {
                        self.removed.remove(&file);
                        self.pending
                            .entry(file)
                            .or_insert_with(|| Pending::found(now, observed, settle));
                    }
                    continue;
                }
                Finding::File { observed, readable } => {
                    let media = is_media(&path);
                    match self.pending.get_mut(&path) {
                        None => continue,
                        Some(_) if !media => Verdict::GiveUp,
                        Some(pending) => pending.after_check(observed, readable, now, settle),
                    }
                }
                Finding::Failed(reason) => {
                    tracing::debug!(path = %path.display(), %reason, "could not check a changed file");
                    match self.pending.get_mut(&path) {
                        None => continue,
                        Some(pending) => pending.after_failed_check(now, settle),
                    }
                }
            };
            match verdict {
                Verdict::Wait => {}
                Verdict::Ready => {
                    let size = self
                        .pending
                        .remove(&path)
                        .and_then(|p| p.observed)
                        .map_or(0, |o| o.size);
                    ready.push((path, size));
                }
                Verdict::GiveUp => {
                    if let Some(pending) = self.pending.remove(&path)
                        && pending.trouble_since.is_some()
                    {
                        tracing::warn!(
                            path = %path.display(),
                            "A changed file still cannot be read, so the watcher stopped \
                             waiting for it; the next scan will pick it up."
                        );
                    }
                }
            }
        }
        ready.sort();
        for (path, size) in ready {
            if self.accepts(&path, size) {
                self.emit(WatchEvent::Upserted(path)).await?;
            }
        }
        Ok(())
    }

    /// Final filter before reporting a settled file.
    fn accepts(&self, path: &Path, size: u64) -> bool {
        let Some((root, entry)) = self.root_of(path) else {
            return false;
        };
        if path.to_str().is_none() {
            tracing::debug!(path = %path.display(), "skipping a file whose name is not valid UTF-8");
            return false;
        }
        let excluded = path
            .strip_prefix(&root)
            .map_or(true, |relative| entry.filter.excludes_file(relative));
        !excluded && size >= entry.filter.min_size_bytes
    }

    /// The watched root containing `path` (the innermost one if nested).
    fn root_of(&self, path: &Path) -> Option<(PathBuf, RootEntry)> {
        lock(&self.shared.roots)
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.as_os_str().len())
            .map(|(root, entry)| (root.clone(), entry.clone()))
    }

    /// Publish how many media files each root has waiting to settle (see
    /// [`WaitingFiles`]), when that changed.
    fn publish_waiting(&mut self, roots: &[(PathBuf, RootEntry)]) {
        let mut counts: HashMap<PathBuf, usize> = HashMap::new();
        for path in self.pending.keys() {
            if !is_media(path) {
                continue;
            }
            if let Some((root, _)) = innermost_root(roots, path) {
                *counts.entry(root.clone()).or_default() += 1;
            }
        }
        if counts != self.published {
            self.waiting.set(counts.clone());
            self.published = counts;
        }
    }

    /// Log a warning unless it was already logged.
    fn warn_once(&mut self, message: String) {
        if self.warned.contains(&message) {
            return;
        }
        tracing::warn!("{message}");
        if self.warned.len() >= MAX_REMEMBERED_WARNINGS {
            self.warned.clear();
        }
        self.warned.insert(message);
    }

    async fn emit(&self, event: WatchEvent) -> Result<(), Closed> {
        tracing::trace!(?event, "folder change");
        self.out.send(event).await.map_err(|_| Closed)
    }
}

/// The innermost root in `roots` containing `path`.
fn innermost_root<'a>(
    roots: &'a [(PathBuf, RootEntry)],
    path: &Path,
) -> Option<&'a (PathBuf, RootEntry)> {
    roots
        .iter()
        .filter(|(root, _)| path.starts_with(root))
        .max_by_key(|(root, _)| root.as_os_str().len())
}

/// Check one pending path below `root` (blocking). A folder that appeared is
/// watched from now on, and its media files are listed.
fn inspect(check: Check, root: &Path, entry: &RootEntry) -> (PathBuf, Finding) {
    let filter = &entry.filter;
    // Anything reached through a linked folder is left alone, as in a walk.
    if !filter.follow_links && through_link(root, &check.path) {
        return (check.path, Finding::NotAFile);
    }
    let metadata = if filter.follow_links {
        std::fs::metadata(&check.path)
    } else {
        std::fs::symlink_metadata(&check.path)
    };
    let finding = match metadata {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Finding::Missing {
                in_existing_folder: check.path.parent().is_some_and(Path::is_dir),
            }
        }
        Err(error) => Finding::Failed(error.to_string()),
        Ok(metadata) if metadata.is_dir() => {
            let walked = match &entry.armed {
                Some(armed) => {
                    let mut watcher = lock(&armed.watcher);
                    walk_folders(root, &check.path, filter, true, true, &mut |folder| {
                        watcher.watch(folder, RecursiveMode::NonRecursive)
                    })
                }
                None => walk_folders(root, &check.path, filter, true, true, &mut |_| Ok(())),
            };
            Finding::Folder(walked)
        }
        // Links (when not followed), devices, sockets and pipes.
        Ok(metadata) if !metadata.is_file() => Finding::NotAFile,
        Ok(metadata) => {
            let observed = Observed {
                size: metadata.len(),
                modified: metadata.modified().ok(),
            };
            let readable = (check.open_if_unchanged && check.previous == Some(observed))
                .then(|| std::fs::File::open(&check.path).is_ok());
            Finding::File { observed, readable }
        }
    };
    (check.path, finding)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_needs_a_runtime() {
        let error = LibraryWatcher::start(Duration::from_secs(1)).unwrap_err();
        assert!(error.to_string().contains("runtime"), "{error}");
    }

    #[test]
    fn limit_errors_explain_the_fix() {
        let error = notify::Error::new(notify::ErrorKind::MaxFilesWatch);
        let message = describe_notify_error(&error, Some(Path::new("/media/movies")));
        assert!(message.contains("/media/movies"), "{message}");
        assert!(message.contains("fs.inotify.max_user_watches"), "{message}");
        assert!(message.contains("rescans still"), "{message}");

        // The library is named rather than the folder where the limit hit.
        let deep = notify::Error::new(notify::ErrorKind::MaxFilesWatch)
            .add_path(PathBuf::from("/media/movies/A/B"));
        let message = describe_notify_error(&deep, Some(Path::new("/media/movies")));
        assert!(message.contains("watch /media/movies for"), "{message}");

        let missing = notify::Error::path_not_found();
        let message = describe_notify_error(&missing, Some(Path::new("/nope")));
        assert!(message.contains("does not exist"), "{message}");

        // Other problems name the folder that failed, not the library root.
        let denied = notify::Error::io(io::Error::from(io::ErrorKind::PermissionDenied))
            .add_path(PathBuf::from("/media/movies/.Trash-0"));
        let message = describe_notify_error(&denied, Some(Path::new("/media/movies")));
        assert!(message.contains("/media/movies/.Trash-0"), "{message}");
        assert!(message.contains("permission"), "{message}");
    }

    #[test]
    fn noise_is_filtered_early() {
        let event = |kind: EventKind, path: &str| Event::new(kind).add_path(PathBuf::from(path));
        let open = EventKind::Access(AccessKind::Open(AccessMode::Any));
        let read_done = EventKind::Access(AccessKind::Close(AccessMode::Read));
        let write_done = EventKind::Access(AccessKind::Close(AccessMode::Write));
        let data = EventKind::Modify(ModifyKind::Data(notify::event::DataChange::Any));
        let created = EventKind::Create(CreateKind::File);
        let folder = EventKind::Create(CreateKind::Folder);
        let removed = EventKind::Remove(RemoveKind::File);

        assert!(is_noise(&event(open, "/m/a.mkv")));
        assert!(is_noise(&event(read_done, "/m/a.mkv")));
        assert!(!is_noise(&event(write_done, "/m/a.mkv")));
        assert!(!is_noise(&event(data, "/m/a.mkv")));
        assert!(!is_noise(&event(created, "/m/a.mkv")));
        assert!(is_noise(&event(data, "/m/a.mkv.part")));
        assert!(is_noise(&event(write_done, "/m/poster.jpg")));
        assert!(is_noise(&event(data, "/m/.a.chrysopoeia-01234567.tmp.mkv")));
        assert!(is_noise(&event(data, "/m/Movie/VIDEO_TS/VTS_01_1.VOB")));
        // Folders and removals always go through.
        assert!(!is_noise(&event(folder, "/m/Season 01")));
        assert!(!is_noise(&event(removed, "/m/poster.jpg")));
    }

    #[test]
    fn pending_settles_only_after_a_quiet_stable_period() {
        let settle = Duration::from_millis(100);
        let t0 = Instant::now();
        let mut pending = Pending::new(t0, false);
        assert!(!pending.settled(t0 + settle * 5, settle), "never observed");

        let first = Observed {
            size: 10,
            modified: None,
        };
        assert!(matches!(
            pending.after_check(first, None, t0, settle),
            Verdict::Wait
        ));
        assert_eq!(pending.next_check, t0 + settle);

        // Grew: the stable period starts over.
        let grown = Observed { size: 20, ..first };
        let t1 = t0 + settle;
        assert!(matches!(
            pending.after_check(grown, None, t1, settle),
            Verdict::Wait
        ));
        assert!(!pending.settled(t1 + settle / 2, settle));
        assert!(pending.settled(t1 + settle, settle));

        // A late event pushes the next check out.
        pending.last_event = t1 + settle / 2;
        assert!(!pending.settled(t1 + settle, settle));
        assert!(matches!(
            pending.after_check(grown, None, t1 + settle, settle),
            Verdict::Wait
        ));
        assert_eq!(pending.next_check, t1 + settle / 2 + settle);

        assert!(matches!(
            pending.after_check(grown, Some(true), t1 + settle * 2, settle),
            Verdict::Ready
        ));
    }

    #[test]
    fn unreadable_files_are_eventually_given_up() {
        let settle = Duration::from_millis(10);
        let t0 = Instant::now();
        let observed = Observed {
            size: 1,
            modified: None,
        };
        let mut pending = Pending::found(t0, observed, settle);
        assert!(matches!(
            pending.after_check(observed, Some(false), t0, settle),
            Verdict::Wait
        ));
        assert!(matches!(
            pending.after_check(observed, Some(false), t0 + MIN_GIVE_UP, settle),
            Verdict::GiveUp
        ));
    }

    fn make_dirs(root: &Path, folders: &[&str]) {
        for folder in folders {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
    }

    fn relative_all(root: &Path, paths: &[PathBuf]) -> Vec<String> {
        let mut names: Vec<String> = paths
            .iter()
            .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn only_folders_a_scan_looks_into_are_watched() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        make_dirs(
            root,
            &[
                "Movies/A/Extras",
                "Movies/Denied/Inner",
                "Movies/Disc (1999)/VIDEO_TS",
                "Movies/Blu (2010)/BDMV/STREAM",
                "#recycle/Old",
                ".Trash-0/files",
                "Movies/@eaDir/A",
                "TV/Show/Season 01",
            ],
        );
        std::fs::write(root.join("Movies/A/A.mkv"), b"12345").unwrap();
        std::fs::write(root.join("Movies/Disc (1999)/VIDEO_TS/VTS_01_1.VOB"), b"x").unwrap();
        std::fs::write(root.join("#recycle/Old/Old.mkv"), b"x").unwrap();
        #[cfg(unix)]
        let _outside = {
            let outside = tempfile::tempdir().unwrap();
            std::fs::create_dir(outside.path().join("Elsewhere")).unwrap();
            std::os::unix::fs::symlink(outside.path(), root.join("Movies/Linked")).unwrap();
            outside
        };

        let filter = RootFilter::defaults();
        let mut watched = Vec::new();
        let walked = walk_folders(root, root, &filter, false, true, &mut |folder| {
            watched.push(folder.to_path_buf());
            if folder.ends_with("Denied") {
                Err(
                    notify::Error::io(io::Error::from(io::ErrorKind::PermissionDenied))
                        .add_path(folder.to_path_buf()),
                )
            } else {
                Ok(())
            }
        });
        assert_eq!(
            relative_all(root, &watched),
            [
                "Movies",
                "Movies/A",
                "Movies/A/Extras",
                "Movies/Blu (2010)",
                "Movies/Denied",
                "Movies/Disc (1999)",
                "TV",
                "TV/Show",
                "TV/Show/Season 01",
            ]
        );
        assert!(walked.limit.is_none());
        assert_eq!(walked.unwatchable.len(), 1);
        assert_eq!(walked.unwatchable[0].0, root.join("Movies/Denied"));
        assert!(walked.unwatchable[0].1.contains("permission"));
        let messages = walked.problem_messages();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("Movies/Denied"), "{messages:?}");
        assert!(messages[0].contains("rest of the library"), "{messages:?}");
        let media: Vec<PathBuf> = walked.media.iter().map(|(p, _)| p.clone()).collect();
        assert_eq!(relative_all(root, &media), ["Movies/A/A.mkv"]);
        assert_eq!(walked.media[0].1.size, 5);

        // A new folder inside an ignored one, or a disc copy, is left alone.
        let mut calls = 0;
        let ignored = walk_folders(
            root,
            &root.join("#recycle/Old"),
            &filter,
            true,
            true,
            &mut |_| {
                calls += 1;
                Ok(())
            },
        );
        let disc = walk_folders(
            root,
            &root.join("Movies/Disc (1999)/VIDEO_TS"),
            &filter,
            true,
            true,
            &mut |_| {
                calls += 1;
                Ok(())
            },
        );
        assert_eq!(calls, 0);
        assert!(ignored.media.is_empty() && disc.media.is_empty());
    }

    #[test]
    fn the_watch_limit_stops_adding_watches() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let folders: Vec<String> = (0..20).map(|i| format!("Show {i:02}/Season 01")).collect();
        let folders: Vec<&str> = folders.iter().map(String::as_str).collect();
        make_dirs(root, &folders);
        let mut calls = 0;
        let walked = walk_folders(
            root,
            root,
            &RootFilter::defaults(),
            false,
            false,
            &mut |p| {
                calls += 1;
                if calls == 5 {
                    Err(notify::Error::new(notify::ErrorKind::MaxFilesWatch)
                        .add_path(p.to_path_buf()))
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(calls, 5, "no more watches after the limit");
        assert!(walked.limit.as_ref().is_some_and(is_limit_error));
        assert!(walked.unwatchable.is_empty());
    }

    #[test]
    fn many_unwatchable_folders_are_summarised() {
        let walked = FolderWalk {
            unwatchable: (0..12)
                .map(|i| {
                    (
                        PathBuf::from(format!("/m/{i}")),
                        "it no longer exists".into(),
                    )
                })
                .collect(),
            ..FolderWalk::default()
        };
        let messages = walked.problem_messages();
        assert_eq!(messages.len(), NAMED_PROBLEMS + 1);
        assert!(
            messages[NAMED_PROBLEMS].starts_with("7 more folders"),
            "{messages:?}"
        );
    }

    #[test]
    fn filters_compare_by_the_folders_they_watch() {
        let defaults = RootFilter::defaults();
        let bigger = RootFilter::new(&ScanOptions {
            min_size_bytes: 100,
            ..ScanOptions::from_settings(&Settings::default())
        });
        assert!(defaults.same_folders(&bigger));
        let other = RootFilter::new(&ScanOptions {
            ignore_patterns: vec!["Extras".into()],
            ..ScanOptions::default()
        });
        assert!(!defaults.same_folders(&other));
        let links = RootFilter::new(&ScanOptions {
            follow_links: true,
            ..ScanOptions::from_settings(&Settings::default())
        });
        assert!(!defaults.same_folders(&links));
        assert!(defaults.excludes_folder(Path::new("Movie/BDMV")));
        assert!(defaults.excludes_file(Path::new("Movie/VIDEO_TS/VTS_01_1.VOB")));
        assert!(defaults.excludes_file(Path::new(".Recycle.Bin/Movie.mkv")));
        assert!(!defaults.excludes_file(Path::new("Movies/Movie.mkv")));
    }
}

/// Tests against the real kernel watcher.
#[cfg(all(test, target_os = "linux"))]
mod live_tests {
    use super::*;
    use chrysopoeia_core::Settings;
    use tokio::io::AsyncWriteExt;

    const SETTLE: Duration = Duration::from_millis(400);
    const WAIT: Duration = Duration::from_secs(10);

    async fn next_event(
        events: &mut mpsc::Receiver<WatchEvent>,
        within: Duration,
    ) -> Option<WatchEvent> {
        tokio::time::timeout(within, events.recv())
            .await
            .ok()
            .flatten()
    }

    /// Everything that arrives within `quiet` of the previous event.
    async fn drain(events: &mut mpsc::Receiver<WatchEvent>, quiet: Duration) -> Vec<WatchEvent> {
        let mut all = Vec::new();
        while let Some(event) = next_event(events, quiet).await {
            all.push(event);
        }
        all
    }

    /// Wait patiently (slow CI machines) for `count` events, then collect
    /// anything else that follows within two settle periods.
    async fn expect(events: &mut mpsc::Receiver<WatchEvent>, count: usize) -> Vec<WatchEvent> {
        let mut all = Vec::new();
        while all.len() < count {
            match next_event(events, WAIT).await {
                Some(event) => all.push(event),
                None => break,
            }
        }
        all.extend(drain(events, SETTLE * 2).await);
        all
    }

    async fn write_file(path: &Path, bytes: usize) {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(path, vec![1u8; bytes]).await.unwrap();
    }

    fn started(root: &Path) -> (LibraryWatcher, mpsc::Receiver<WatchEvent>) {
        let (watcher, events) = LibraryWatcher::start(SETTLE).unwrap();
        watcher.watch(root).unwrap();
        (watcher, events)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slow_copy_is_reported_once_after_settling() {
        let dir = tempfile::tempdir().unwrap();
        let (_watcher, mut events) = started(dir.path());
        let path = dir.path().join("Movie (2020).mkv");

        let mut file = tokio::fs::File::create(&path).await.unwrap();
        for _ in 0..6 {
            file.write_all(&[7u8; 64 * 1024]).await.unwrap();
            file.flush().await.unwrap();
            tokio::time::sleep(SETTLE / 4).await;
            assert!(events.try_recv().is_err(), "reported while still copying");
        }
        file.sync_all().await.unwrap();
        drop(file);
        let finished = Instant::now();

        let first = next_event(&mut events, WAIT).await;
        assert_eq!(first, Some(WatchEvent::Upserted(path.clone())));
        assert!(
            finished.elapsed() >= SETTLE * 3 / 4,
            "reported {:?} after the copy finished",
            finished.elapsed()
        );
        assert_eq!(next_event(&mut events, SETTLE * 3).await, None);
    }

    /// A copy in progress counts as a file waited on for its root (so the
    /// library can say it is waiting for it), until it is reported.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn copies_in_progress_are_counted_per_root() {
        let dir = tempfile::tempdir().unwrap();
        let (watcher, mut events) = started(dir.path());
        let waiting = watcher.waiting_files();
        assert!(waiting.counts().is_empty());
        let path = dir.path().join("Movie (2020).mkv");
        let mut file = tokio::fs::File::create(&path).await.unwrap();
        let mut seen = 0;
        for _ in 0..8 {
            file.write_all(&[7u8; 64 * 1024]).await.unwrap();
            file.flush().await.unwrap();
            tokio::time::sleep(SETTLE / 4).await;
            seen = seen.max(waiting.counts().get(dir.path()).copied().unwrap_or(0));
        }
        assert_eq!(seen, 1, "{:?}", waiting.counts());
        // Subtitles aren't media: never counted.
        write_file(&dir.path().join("Movie (2020).srt"), 10).await;
        file.sync_all().await.unwrap();
        drop(file);
        assert_eq!(
            next_event(&mut events, WAIT).await,
            Some(WatchEvent::Upserted(path))
        );
        let deadline = Instant::now() + WAIT;
        while !waiting.counts().is_empty() && Instant::now() < deadline {
            tokio::time::sleep(SETTLE / 8).await;
        }
        assert!(waiting.counts().is_empty(), "{:?}", waiting.counts());
        drop(watcher);
        assert!(waiting.counts().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn deletions_are_reported_for_files_and_folders() {
        let dir = tempfile::tempdir().unwrap();
        let (_watcher, mut events) = started(dir.path());
        let movie = dir.path().join("Movie.mkv");
        let season = dir.path().join("Show/Season 01");
        let episode = season.join("Episode 1.mkv");
        write_file(&movie, 1000).await;
        write_file(&episode, 1000).await;
        write_file(&season.join("Episode 1.srt"), 10).await;

        let mut settled = expect(&mut events, 2).await;
        settled.sort_by_key(|e| format!("{e:?}"));
        assert_eq!(
            settled,
            [
                WatchEvent::Upserted(movie.clone()),
                WatchEvent::Upserted(episode.clone())
            ]
        );

        tokio::fs::remove_file(&movie).await.unwrap();
        assert_eq!(
            next_event(&mut events, WAIT).await,
            Some(WatchEvent::Removed(movie))
        );

        tokio::fs::remove_dir_all(&season).await.unwrap();
        let removed = expect(&mut events, 2).await;
        assert!(
            removed.contains(&WatchEvent::Removed(season.clone())),
            "{removed:?}"
        );
        assert!(
            removed.contains(&WatchEvent::Removed(episode)),
            "{removed:?}"
        );
        let unique: std::collections::HashSet<String> =
            removed.iter().map(|e| format!("{e:?}")).collect();
        assert_eq!(unique.len(), removed.len(), "duplicates: {removed:?}");
        assert!(
            !removed.iter().any(
                |e| matches!(e, WatchEvent::Removed(p) if p.extension().is_some_and(|x| x == "srt"))
            ),
            "{removed:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn renames_remove_the_old_name_and_add_the_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let (_watcher, mut events) = started(dir.path());
        let old = dir.path().join("old name.mkv");
        let new = dir.path().join("New Name (2021).mkv");
        write_file(&old, 1000).await;
        assert_eq!(
            next_event(&mut events, WAIT).await,
            Some(WatchEvent::Upserted(old.clone()))
        );

        tokio::fs::rename(&old, &new).await.unwrap();
        assert_eq!(
            expect(&mut events, 2).await,
            [WatchEvent::Removed(old), WatchEvent::Upserted(new.clone())]
        );

        // A download client finishing: partial name -> real name.
        let partial = dir.path().join("Download.mkv.part");
        let done = dir.path().join("Download.mkv");
        write_file(&partial, 1000).await;
        tokio::fs::rename(&partial, &done).await.unwrap();
        assert_eq!(expect(&mut events, 1).await, [WatchEvent::Upserted(done)]);

        // Moved out of the library entirely.
        let outside = tempfile::tempdir().unwrap();
        tokio::fs::rename(&new, outside.path().join("elsewhere.mkv"))
            .await
            .unwrap();
        assert_eq!(expect(&mut events, 1).await, [WatchEvent::Removed(new)]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn artifacts_and_sidecars_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let (_watcher, mut events) = started(dir.path());
        let folder = dir.path().join("Movie (2020)");
        let movie = folder.join("Movie (2020).mkv");
        write_file(&movie, 1000).await;
        write_file(&folder.join("Movie (2020).nfo"), 10).await;
        write_file(&folder.join("poster.jpg"), 10).await;
        write_file(&folder.join("Movie (2020).en.srt"), 10).await;
        assert_eq!(
            expect(&mut events, 1).await,
            [WatchEvent::Upserted(movie.clone())]
        );

        // What finalize does when replacing a file in place.
        let id = uuid::Uuid::from_u128(0xfeed_beef);
        let temp = folder.join(chrysopoeia_core::paths::temp_file_name(
            "Movie (2020)",
            id,
            "mkv",
        ));
        let backup = folder.join(chrysopoeia_core::paths::backup_file_name(
            "Movie (2020).mkv",
            id,
        ));
        write_file(&temp, 700).await;
        tokio::fs::rename(&movie, &backup).await.unwrap();
        tokio::fs::rename(&temp, &movie).await.unwrap();
        tokio::fs::remove_file(&backup).await.unwrap();
        assert_eq!(
            expect(&mut events, 1).await,
            [WatchEvent::Upserted(movie.clone())]
        );

        // Removing sidecars and a leftover temp file reports nothing.
        write_file(&temp, 10).await;
        tokio::fs::remove_file(&temp).await.unwrap();
        tokio::fs::remove_file(folder.join("poster.jpg"))
            .await
            .unwrap();
        assert_eq!(drain(&mut events, SETTLE * 3).await, []);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn folders_that_appear_are_looked_into() {
        let dir = tempfile::tempdir().unwrap();
        let (_watcher, mut events) = started(dir.path());

        // A whole season moved in from elsewhere on the same disk.
        let staging = tempfile::tempdir_in(dir.path().parent().unwrap()).unwrap();
        let prepared = staging.path().join("Season 02");
        write_file(&prepared.join("E01.mkv"), 1000).await;
        write_file(&prepared.join("Extras/E01 Commentary.mka"), 1000).await;
        write_file(&prepared.join("E01.srt"), 10).await;
        let season = dir.path().join("Show/Season 02");
        tokio::fs::create_dir_all(season.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::rename(&prepared, &season).await.unwrap();

        let mut arrived = expect(&mut events, 2).await;
        arrived.sort_by_key(|e| format!("{e:?}"));
        assert_eq!(
            arrived,
            [
                WatchEvent::Upserted(season.join("E01.mkv")),
                WatchEvent::Upserted(season.join("Extras/E01 Commentary.mka")),
            ]
        );

        // A folder created and filled right away.
        let fresh = dir.path().join("New/Deep/Folder");
        write_file(&fresh.join("A.mp4"), 1000).await;
        write_file(&fresh.join("B.mp4"), 1000).await;
        let mut arrived = expect(&mut events, 2).await;
        arrived.sort_by_key(|e| format!("{e:?}"));
        assert_eq!(
            arrived,
            [
                WatchEvent::Upserted(fresh.join("A.mp4")),
                WatchEvent::Upserted(fresh.join("B.mp4")),
            ]
        );

        // Folders that appeared are watched from then on, however deep.
        write_file(&fresh.join("C.mp4"), 1000).await;
        write_file(&season.join("Extras/E02 Commentary.mka"), 1000).await;
        let mut arrived = expect(&mut events, 2).await;
        arrived.sort_by_key(|e| format!("{e:?}"));
        assert_eq!(
            arrived,
            [
                WatchEvent::Upserted(fresh.join("C.mp4")),
                WatchEvent::Upserted(season.join("Extras/E02 Commentary.mka")),
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn folders_moved_within_the_library_stay_watched() {
        let dir = tempfile::tempdir().unwrap();
        let before = dir.path().join("Show/Season 1");
        write_file(&before.join("Deep/E01.mkv"), 1000).await;
        let (_watcher, mut events) = started(dir.path());

        let after = dir.path().join("Show/Season 01");
        tokio::fs::rename(&before, &after).await.unwrap();
        let mut moved = expect(&mut events, 2).await;
        moved.sort_by_key(|e| format!("{e:?}"));
        assert_eq!(
            moved,
            [
                WatchEvent::Removed(before.clone()),
                WatchEvent::Upserted(after.join("Deep/E01.mkv")),
            ]
        );

        // The moved folders (and the ones inside) are watched under their
        // new names; the old names are gone.
        write_file(&after.join("E02.mkv"), 1000).await;
        write_file(&after.join("Deep/E03.mkv"), 1000).await;
        let mut arrived = expect(&mut events, 2).await;
        arrived.sort_by_key(|e| format!("{e:?}"));
        assert_eq!(
            arrived,
            [
                WatchEvent::Upserted(after.join("Deep/E03.mkv")),
                WatchEvent::Upserted(after.join("E02.mkv")),
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plain_watch_ignores_nas_and_mac_clutter_like_a_walk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let (_watcher, mut events) = started(root);
        let clutter = [
            "#recycle/Old.mkv",
            ".Recycle.Bin/Movies/Deleted.mkv",
            ".Trash-1000/files/Trashed.mkv",
            "Movies/._Movie (2020).mkv",
            "Movies/@eaDir/Movie (2020).mkv/SYNOPHOTO_FILM_M.mp4",
            "Movies/Download.mkv.partial~",
        ];
        for name in clutter {
            write_file(&root.join(name), 1000).await;
        }
        let movie = root.join("Movies/Movie (2020).mkv");
        write_file(&movie, 1000).await;
        assert_eq!(
            expect(&mut events, 1).await,
            [WatchEvent::Upserted(movie.clone())]
        );

        // Exactly what a walk with the default settings lists.
        let opts = ScanOptions::from_settings(&Settings::default());
        let walked = crate::walk_library(root, &opts).unwrap();
        let listed: Vec<PathBuf> = walked.files.into_iter().map(|f| f.path).collect();
        assert_eq!(listed, [movie]);

        // Deleting inside an ignored folder reports nothing; deleting the
        // ignored folder itself reports only that folder (harmless: nothing
        // below it is in the library).
        tokio::fs::remove_file(root.join("#recycle/Old.mkv"))
            .await
            .unwrap();
        assert_eq!(drain(&mut events, SETTLE * 2).await, []);
        tokio::fs::remove_dir_all(root.join(".Recycle.Bin"))
            .await
            .unwrap();
        assert_eq!(
            drain(&mut events, SETTLE * 2).await,
            [WatchEvent::Removed(root.join(".Recycle.Bin"))]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disc_copies_are_never_reported() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // An existing disc copy, and one copied in while watching.
        write_file(&root.join("Old Disc (1999)/VIDEO_TS/VIDEO_TS.IFO"), 10).await;
        let (_watcher, mut events) = started(root);
        write_file(&root.join("Old Disc (1999)/VIDEO_TS/VTS_01_1.VOB"), 1000).await;
        write_file(&root.join("Movie A (1999)/VIDEO_TS/VTS_01_1.VOB"), 1000).await;
        write_file(&root.join("Movie A (1999)/VIDEO_TS/VTS_01_2.VOB"), 1000).await;
        write_file(&root.join("Movie B (2010)/BDMV/STREAM/00000.m2ts"), 1000).await;
        write_file(&root.join("Movie B (2010)/BDMV/index.bdmv"), 10).await;
        let movie = root.join("Movie C (2020)/Movie C (2020).mkv");
        write_file(&movie, 1000).await;
        assert_eq!(expect(&mut events, 1).await, [WatchEvent::Upserted(movie)]);
        // Later writes inside the disc folders are not seen either.
        write_file(&root.join("Movie A (1999)/VIDEO_TS/VTS_01_3.VOB"), 1000).await;
        assert_eq!(drain(&mut events, SETTLE * 3).await, []);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn links_are_left_alone_like_a_walk() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path();
        write_file(&outside.path().join("real.mkv"), 1000).await;
        tokio::fs::create_dir_all(outside.path().join("ext"))
            .await
            .unwrap();
        let (_watcher, mut events) = started(root);
        tokio::fs::create_dir_all(root.join("Movies"))
            .await
            .unwrap();
        tokio::fs::symlink(outside.path().join("ext"), root.join("Movies/LinkedDir"))
            .await
            .unwrap();
        tokio::fs::symlink(
            outside.path().join("real.mkv"),
            root.join("Movies/symlinked-file.mkv"),
        )
        .await
        .unwrap();
        write_file(&outside.path().join("ext/outside.mkv"), 1000).await;
        let real = root.join("Movies/Real.mkv");
        write_file(&real, 1000).await;
        assert_eq!(
            expect(&mut events, 1).await,
            [WatchEvent::Upserted(real.clone())]
        );

        let walked = crate::walk_library(root, &ScanOptions::default()).unwrap();
        let listed: Vec<PathBuf> = walked.files.into_iter().map(|f| f.path).collect();
        assert_eq!(listed, [real]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_relative_root_is_watched() {
        let dir = tempfile::tempdir().unwrap();
        // A path from the current directory to the temporary folder.
        let cwd = std::env::current_dir().unwrap();
        let mut relative = PathBuf::new();
        for _ in cwd.components().skip(1) {
            relative.push("..");
        }
        relative.push(dir.path().strip_prefix("/").unwrap());
        assert!(relative.is_relative());

        let (watcher, mut events) = LibraryWatcher::start(SETTLE).unwrap();
        watcher.watch(&relative).unwrap();
        let expected = std::path::absolute(&relative).unwrap().join("a.mkv");
        write_file(&dir.path().join("a.mkv"), 10).await;
        assert_eq!(
            expect(&mut events, 1).await,
            [WatchEvent::Upserted(expected)]
        );

        watcher.unwatch(&relative).unwrap();
        write_file(&dir.path().join("b.mkv"), 10).await;
        assert_eq!(drain(&mut events, SETTLE * 3).await, []);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn waiting_to_watch_does_not_block_the_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let (watcher, mut events) = LibraryWatcher::start(SETTLE).unwrap();
        let watcher = Arc::new(watcher);

        // Another root being set up (a large library on slow disks) keeps
        // the watcher busy for a while.
        let (busy_tx, busy_rx) = std::sync::mpsc::channel();
        let other_root = {
            let shared = Arc::clone(&watcher.shared);
            std::thread::spawn(move || {
                let _arming = lock(&shared.arming);
                busy_tx.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(800));
            })
        };
        busy_rx.recv().unwrap();

        // Records when the runtime's only worker thread was free.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ticker = {
            let stop = Arc::clone(&stop);
            tokio::spawn(async move {
                let mut ticks = Vec::new();
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    ticks.push(Instant::now());
                }
                ticks
            })
        };
        let started = Instant::now();
        let waiting = {
            let watcher = Arc::clone(&watcher);
            let root = dir.path().to_path_buf();
            tokio::spawn(async move {
                watcher.watch(&root).unwrap();
                Instant::now()
            })
        };
        let done = waiting.await.unwrap();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let ticks = ticker.await.unwrap();
        other_root.join().unwrap();

        assert!(
            done - started >= Duration::from_millis(500),
            "watch() did not wait"
        );
        let (from, to) = (started + Duration::from_millis(100), done);
        let free = ticks.iter().filter(|&&t| t > from && t < to).count();
        assert!(
            free >= 20,
            "the runtime was blocked while watch() waited ({free} ticks in {:?})",
            to - from
        );

        // And the root is watched once the wait is over.
        let movie = dir.path().join("Movie.mkv");
        write_file(&movie, 10).await;
        assert_eq!(expect(&mut events, 1).await, [WatchEvent::Upserted(movie)]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn roots_armed_at_the_same_time_are_both_watched() {
        let big = tempfile::tempdir().unwrap();
        for show in 0..40 {
            for season in 0..50 {
                std::fs::create_dir_all(big.path().join(format!("S{show}/{season}"))).unwrap();
            }
        }
        let small = tempfile::tempdir().unwrap();
        let (watcher, mut events) = LibraryWatcher::start(SETTLE).unwrap();
        let watcher = Arc::new(watcher);
        let calls: Vec<_> = [big.path(), small.path(), big.path()]
            .into_iter()
            .map(|root| {
                let watcher = Arc::clone(&watcher);
                let root = root.to_path_buf();
                tokio::spawn(async move { watcher.watch(&root) })
            })
            .collect();
        for call in calls {
            call.await.unwrap().unwrap();
        }
        let deep = big.path().join("S39/49/Episode.mkv");
        let flat = small.path().join("Movie.mkv");
        write_file(&deep, 10).await;
        write_file(&flat, 10).await;
        let mut arrived = expect(&mut events, 2).await;
        arrived.sort_by_key(|e| format!("{e:?}"));
        let mut expected = vec![WatchEvent::Upserted(deep), WatchEvent::Upserted(flat)];
        expected.sort_by_key(|e| format!("{e:?}"));
        assert_eq!(arrived, expected);
    }

    /// Mounts a tmpfs on a folder and unmounts it again when dropped.
    struct Tmpfs(PathBuf);

    impl Tmpfs {
        fn mount(at: &Path) -> Option<Self> {
            let mounted = std::process::Command::new("mount")
                .args(["-t", "tmpfs", "-o", "size=16m", "chrysopoeia-test"])
                .arg(at)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|status| status.success());
            mounted.then(|| Self(at.to_path_buf()))
        }

        fn unmount(self) {
            // Drop does the work.
        }
    }

    impl Drop for Tmpfs {
        fn drop(&mut self) {
            let _ = std::process::Command::new("umount")
                .arg("-l")
                .arg(&self.0)
                .status();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_remounted_root_is_watched_again() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("disk");
        std::fs::create_dir(&root).unwrap();
        let Some(disk) = Tmpfs::mount(&root) else {
            eprintln!("skipping: mounting a tmpfs is not permitted here");
            return;
        };
        let (_watcher, mut events) = started(&root);
        let first = root.join("Movies/A.mkv");
        write_file(&first, 10).await;
        assert_eq!(expect(&mut events, 1).await, [WatchEvent::Upserted(first)]);

        // The disk goes away and comes back (a new filesystem).
        disk.unmount();
        let Some(_disk) = Tmpfs::mount(&root) else {
            eprintln!("skipping: mounting a tmpfs again is not permitted here");
            return;
        };
        // Nobody calls watch() again: the watcher notices by itself. Keep
        // adding files until one is reported.
        let mut seen = Vec::new();
        for attempt in 0..20 {
            write_file(&root.join(format!("Movies/B{attempt}.mkv")), 10).await;
            seen.extend(drain(&mut events, Duration::from_millis(700)).await);
            if seen.iter().any(|e| matches!(e, WatchEvent::Upserted(_))) {
                break;
            }
        }
        assert!(
            seen.iter().any(|e| matches!(e, WatchEvent::Upserted(_))),
            "nothing reported after the disk came back: {seen:?}"
        );
        // Unmounting never looks like deleted files.
        assert!(
            !seen.iter().any(|e| matches!(e, WatchEvent::Removed(_))),
            "{seen:?}"
        );
    }

    /// Set by [`an_unreadable_subfolder_does_not_stop_watching`] when it runs
    /// the scenario again as an ordinary user.
    const UNPRIVILEGED_ROOT: &str = "CHRYSOPOEIA_TEST_UNPRIVILEGED_WATCH_ROOT";

    /// Printed by the ordinary-user run once the scenario passed.
    const SCENARIO_DONE: &str = "unreadable-folder scenario passed as an ordinary user";

    fn running_as_root() -> bool {
        std::fs::read_to_string("/proc/self/status").is_ok_and(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Uid:"))
                .and_then(|ids| ids.split_whitespace().nth(1))
                == Some("0")
        })
    }

    /// A library with a folder the watcher may not read (a root-owned
    /// `.Trash-0` and a private folder), watched by an ordinary user.
    async fn unreadable_folder_scenario(root: &Path) {
        let (watcher, mut events) = LibraryWatcher::start(SETTLE).unwrap();
        watcher
            .watch(root)
            .expect("one unreadable folder must not stop the whole library being watched");
        let movie = root.join("Movies/A.mkv");
        write_file(&movie, 10).await;
        assert_eq!(expect(&mut events, 1).await, [WatchEvent::Upserted(movie)]);
        let later = root.join("Movies/New/B.mkv");
        write_file(&later, 10).await;
        assert_eq!(expect(&mut events, 1).await, [WatchEvent::Upserted(later)]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unreadable_subfolder_does_not_stop_watching() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        let mode = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        };
        for folder in ["Movies", ".Trash-0/files", "Private/Inner"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
        }
        mode(dir.path(), 0o755);
        mode(&root, 0o777);
        mode(&root.join("Movies"), 0o777);
        mode(&root.join(".Trash-0"), 0o700);
        mode(&root.join("Private"), 0o700);
        if !running_as_root() {
            // Nothing the current user owns can be made unreadable to itself
            // except by mode 000.
            mode(&root.join("Private"), 0o000);
            unreadable_folder_scenario(&root).await;
            mode(&root.join("Private"), 0o700);
            return;
        }
        // Root reads everything: run the scenario again as an ordinary user
        // (uid 99, like Unraid's "nobody").
        let exe = std::env::current_exe().unwrap();
        let output = tokio::process::Command::new("setpriv")
            .args(["--reuid=99", "--regid=100", "--clear-groups", "--"])
            .arg(exe)
            .args([
                "--exact",
                "watch::live_tests::unreadable_folder_as_an_ordinary_user",
                "--nocapture",
            ])
            .env(UNPRIVILEGED_ROOT, &root)
            .output()
            .await;
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                eprintln!("skipping: setpriv is not available ({error})");
                return;
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() && stderr.starts_with("setpriv:") {
            eprintln!("skipping: cannot switch users here ({stderr})");
            return;
        }
        assert!(
            output.status.success() && stderr.contains(SCENARIO_DONE),
            "as uid 99:\n{stdout}\n{stderr}"
        );
    }

    /// Only does something when started by
    /// [`an_unreadable_subfolder_does_not_stop_watching`].
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unreadable_folder_as_an_ordinary_user() {
        let Some(root) = std::env::var_os(UNPRIVILEGED_ROOT) else {
            return;
        };
        assert!(!running_as_root(), "meant to run as an ordinary user");
        unreadable_folder_scenario(Path::new(&root)).await;
        eprintln!("{SCENARIO_DONE}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn options_filter_like_a_walk() {
        let dir = tempfile::tempdir().unwrap();
        let (watcher, mut events) = LibraryWatcher::start(SETTLE).unwrap();
        let opts = ScanOptions {
            ignore_patterns: chrysopoeia_core::Settings::default().ignore_patterns,
            min_size_bytes: 100,
            follow_links: false,
        };
        watcher.watch_with_options(dir.path(), &opts).unwrap();
        write_file(&dir.path().join("Samples/sample.mkv"), 10).await;
        write_file(&dir.path().join(".hidden/Movie.mkv"), 1000).await;
        write_file(&dir.path().join("#recycle/Movie.mkv"), 1000).await;
        write_file(&dir.path().join("Movie.mkv"), 1000).await;
        assert_eq!(
            expect(&mut events, 1).await,
            [WatchEvent::Upserted(dir.path().join("Movie.mkv"))]
        );

        // Updating the options of a watched root applies right away.
        let stricter = ScanOptions {
            ignore_patterns: vec!["Movies".into()],
            ..ScanOptions::default()
        };
        watcher.watch_with_options(dir.path(), &stricter).unwrap();
        write_file(&dir.path().join("Movies/A.mkv"), 1000).await;
        write_file(&dir.path().join("Small.mkv"), 10).await;
        assert_eq!(
            expect(&mut events, 1).await,
            [WatchEvent::Upserted(dir.path().join("Small.mkv"))]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watch_and_unwatch_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let (watcher, mut events) = started(dir.path());
        watcher.watch(dir.path()).unwrap();
        watcher.unwatch(&dir.path().join("never watched")).unwrap();

        let missing = watcher.watch(&dir.path().join("missing")).unwrap_err();
        assert!(missing.to_string().contains("does not exist"), "{missing}");
        let file = dir.path().join("file.mkv");
        write_file(&file, 10).await;
        let not_dir = watcher.watch(&file).unwrap_err();
        assert!(not_dir.to_string().contains("not a folder"), "{not_dir}");
        assert_eq!(expect(&mut events, 1).await, [WatchEvent::Upserted(file)]);

        watcher.unwatch(dir.path()).unwrap();
        watcher.unwatch(dir.path()).unwrap();
        write_file(&dir.path().join("after.mkv"), 10).await;
        assert_eq!(drain(&mut events, SETTLE * 3).await, []);

        watcher.watch(dir.path()).unwrap();
        write_file(&dir.path().join("again.mkv"), 10).await;
        assert_eq!(
            expect(&mut events, 1).await,
            [WatchEvent::Upserted(dir.path().join("again.mkv"))]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_deleted_root_is_not_reported_and_can_be_rewatched() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("library");
        write_file(&root.join("Movie.mkv"), 10).await;
        let (watcher, mut events) = started(&root);

        tokio::fs::remove_dir_all(&root).await.unwrap();
        let events_seen = drain(&mut events, SETTLE * 2).await;
        assert!(
            !events_seen.contains(&WatchEvent::Removed(root.clone())),
            "{events_seen:?}"
        );

        write_file(&root.join("Back.mkv"), 10).await;
        watcher.watch(&root).unwrap();
        write_file(&root.join("New.mkv"), 10).await;
        assert!(
            expect(&mut events, 1)
                .await
                .contains(&WatchEvent::Upserted(root.join("New.mkv")))
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_the_watcher_ends_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        let (watcher, mut events) = started(dir.path());
        drop(watcher);
        let end = tokio::time::timeout(WAIT, events.recv()).await;
        assert_eq!(end.ok(), Some(None));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_bulk_import_is_reported_once_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_watcher, mut events) = started(dir.path());
        let mut expected = Vec::new();
        for season in 1..=5 {
            for episode in 1..=100 {
                let path = dir.path().join(format!(
                    "Show/Season {season:02}/S{season:02}E{episode:03}.mkv"
                ));
                write_file(&path, 64).await;
                // Rewrite a few to check that bursts coalesce.
                if episode % 10 == 0 {
                    write_file(&path, 128).await;
                }
                write_file(&path.with_extension("srt"), 8).await;
                expected.push(WatchEvent::Upserted(path));
            }
        }
        let mut arrived = expect(&mut events, 500).await;
        arrived.sort_by_key(|e| format!("{e:?}"));
        expected.sort_by_key(|e| format!("{e:?}"));
        assert_eq!(arrived.len(), expected.len());
        assert_eq!(arrived, expected);
    }
}
