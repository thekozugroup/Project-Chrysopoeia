//! Debounced folder watching.
//!
//! notify delivers raw filesystem events on its own thread (inotify on
//! Linux). Its callback only forwards them over a channel; a tokio task, the
//! debouncer, coalesces bursts, pairs renames, waits for copies to finish and
//! emits [`WatchEvent`]s. Every filesystem check the debouncer makes runs on
//! the blocking pool, batched once per tick, so the runtime never blocks.
//!
//! # When a file counts as settled
//!
//! A changed media file is reported as [`WatchEvent::Upserted`] once no event
//! has arrived for it for the settle period, the same size and modification
//! time have been seen at least a settle period apart, and it can be opened
//! for reading. While a copy is running the file is checked at most about
//! once per settle period, so thousands of files arriving at once stay cheap.
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
//!   reported (an unmounted disk must not empty a library). The root simply
//!   stops being watched until [`LibraryWatcher::watch`] is called again.
//!
//! Limits: network shares (SMB/NFS) and some container setups deliver no
//! change events for changes made elsewhere; periodic rescans cover those.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use anyhow::anyhow;
use notify::event::{AccessKind, AccessMode, CreateKind, ModifyKind, RemoveKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::runtime::{Handle, RuntimeFlavor};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use walkdir::WalkDir;

use crate::ScanOptions;
use crate::walk::{IgnoreRules, SIDECAR_EXTENSIONS, has_extension_in, is_artifact_path, is_media};

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

/// A settled change under a watched root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// A media file was created or modified and its size has stopped
    /// changing for the settle period (i.e. the copy finished).
    Upserted(PathBuf),
    /// A media file (or a folder containing media) was removed or moved away.
    Removed(PathBuf),
}

/// How events under one root are filtered.
#[derive(Debug, Default)]
struct RootFilter {
    rules: IgnoreRules,
    min_size_bytes: u64,
}

/// Watched roots and their filters, shared with the debouncer.
type Roots = Arc<Mutex<HashMap<PathBuf, Arc<RootFilter>>>>;

/// Lock a mutex, carrying on if a thread panicked while holding it (the
/// guarded data stays consistent: every critical section is a single map
/// operation).
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Watches library roots. Dropping it stops watching.
pub struct LibraryWatcher {
    watcher: Mutex<RecommendedWatcher>,
    roots: Roots,
    task: JoinHandle<()>,
}

impl fmt::Debug for LibraryWatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let roots: Vec<PathBuf> = lock(&self.roots).keys().cloned().collect();
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
        let watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
            if result.as_ref().is_ok_and(is_noise) {
                return;
            }
            // Fails only once the debouncer is gone, i.e. while shutting down.
            let _ = raw_tx.send(result);
        })
        .map_err(|error| anyhow!(describe_notify_error(&error, None)))?;

        let (out_tx, out_rx) = mpsc::channel(EVENT_BUFFER);
        let roots = Roots::default();
        let debouncer = Debouncer::new(settle, Arc::clone(&roots), out_tx);
        let task = runtime.spawn(debouncer.run(raw_rx));
        let watcher = Self {
            watcher: Mutex::new(watcher),
            roots,
            task,
        };
        Ok((watcher, out_rx))
    }

    /// Start watching a root recursively. Watching the same root twice is a no-op.
    ///
    /// Only Chrysopoeia's own files and non-media files are filtered out;
    /// use [`LibraryWatcher::watch_with_options`] to also apply the
    /// library's ignore patterns and minimum size. Calling this again after
    /// the root folder was deleted or unmounted re-arms it.
    pub fn watch(&self, root: &Path) -> anyhow::Result<()> {
        self.add_root(root, None)
    }

    /// Like [`LibraryWatcher::watch`], but events are also filtered with the
    /// ignore patterns and minimum size in `opts`, exactly as
    /// [`crate::walk_library`] would filter them. Calling it for a root that
    /// is already watched just updates the filter.
    pub fn watch_with_options(&self, root: &Path, opts: &ScanOptions) -> anyhow::Result<()> {
        let filter = RootFilter {
            rules: IgnoreRules::new(&opts.ignore_patterns),
            min_size_bytes: opts.min_size_bytes,
        };
        self.add_root(root, Some(filter))
    }

    fn add_root(&self, root: &Path, filter: Option<RootFilter>) -> anyhow::Result<()> {
        // The watcher lock serializes watch/unwatch calls.
        let mut watcher = lock(&self.watcher);
        if let Some(existing) = lock(&self.roots).get_mut(root) {
            if let Some(filter) = filter {
                *existing = Arc::new(filter);
            }
            return Ok(());
        }
        run_blocking(|| -> anyhow::Result<()> {
            check_watch_root(root)?;
            // Clear watches left behind when this root was deleted or moved
            // while it was watched. Fails harmlessly when there are none.
            let _ = watcher.unwatch(root);
            if let Err(error) = watcher.watch(root, RecursiveMode::Recursive) {
                // Remove a partial recursive watch (for example when the
                // inotify limit was reached halfway) so a retry starts clean.
                let _ = watcher.unwatch(root);
                return Err(anyhow!(describe_notify_error(&error, Some(root))));
            }
            Ok(())
        })?;
        lock(&self.roots).insert(root.to_path_buf(), Arc::new(filter.unwrap_or_default()));
        tracing::debug!(root = %root.display(), "watching folder for changes");
        Ok(())
    }

    /// Stop watching a root. Unknown roots are a no-op.
    pub fn unwatch(&self, root: &Path) -> anyhow::Result<()> {
        let mut watcher = lock(&self.watcher);
        if lock(&self.roots).remove(root).is_none() {
            return Ok(());
        }
        if let Err(error) = run_blocking(|| watcher.unwatch(root)) {
            // The kernel already dropped the watches of a deleted folder;
            // events for this root are filtered out either way.
            tracing::debug!(root = %root.display(), %error, "unwatching reported an error");
        }
        Ok(())
    }
}

impl Drop for LibraryWatcher {
    fn drop(&mut self) {
        self.task.abort();
    }
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

fn check_watch_root(root: &Path) -> anyhow::Result<()> {
    let shown = root.display();
    match std::fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
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

/// Explain a notify error in plain language, with the fix when there is one.
fn describe_notify_error(error: &notify::Error, root: Option<&Path>) -> String {
    let folder = root
        .or_else(|| error.paths.first().map(PathBuf::as_path))
        .map_or_else(
            || "the library folders".to_string(),
            |p| p.display().to_string(),
        );
    match &error.kind {
        notify::ErrorKind::MaxFilesWatch => watch_limit_message(&folder),
        notify::ErrorKind::Io(io_error) if is_errno(io_error, ENOSPC) => {
            watch_limit_message(&folder)
        }
        notify::ErrorKind::Io(io_error) if is_errno(io_error, EMFILE) => format!(
            "Cannot watch {folder} for changes: the system limit on folder watchers \
             (fs.inotify.max_user_instances) has been reached. Raise it on the host, for example \
             with `sysctl -w fs.inotify.max_user_instances=512`. Periodic rescans still find new \
             and changed files."
        ),
        notify::ErrorKind::Io(io_error) if io_error.kind() == io::ErrorKind::PermissionDenied => {
            format!(
                "Chrysopoeia does not have permission to watch {folder} for changes. Periodic \
                 rescans still find new and changed files."
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
    Missing,
    NotAFile,
    /// A folder, with the media files inside it.
    Folder(Vec<(PathBuf, Observed)>),
    File {
        observed: Observed,
        /// Whether it could be opened, when that was tried.
        readable: Option<bool>,
    },
    Failed(String),
}

/// The receiver of watch events was dropped.
struct Closed;

struct Debouncer {
    settle: Duration,
    roots: Roots,
    out: mpsc::Sender<WatchEvent>,
    pending: HashMap<PathBuf, Pending>,
    /// Rename sources waiting for their destination, by rename cookie.
    renames: HashMap<usize, (PathBuf, Instant)>,
    /// Recently reported removals, to drop duplicates within a burst.
    removed: HashMap<PathBuf, Instant>,
}

impl Debouncer {
    fn new(settle: Duration, roots: Roots, out: mpsc::Sender<WatchEvent>) -> Self {
        Self {
            settle,
            roots,
            out,
            pending: HashMap::new(),
            renames: HashMap::new(),
            removed: HashMap::new(),
        }
    }

    async fn run(mut self, mut raw: mpsc::UnboundedReceiver<notify::Result<Event>>) {
        let mut ticker = tokio::time::interval((self.settle / 4).clamp(MIN_TICK, MAX_TICK));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let out = self.out.clone();
        loop {
            let step = tokio::select! {
                received = raw.recv() => match received {
                    Some(Ok(event)) => self.on_event(event).await,
                    Some(Err(error)) => {
                        tracing::warn!("{}", describe_notify_error(&error, None));
                        Ok(())
                    }
                    None => break,
                },
                _ = ticker.tick() => self.on_tick().await,
                () = out.closed() => break,
            };
            if step.is_err() {
                break;
            }
        }
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
            let to_is_dir = tokio::fs::symlink_metadata(&to)
                .await
                .ok()
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
        let Some((root, filter)) = self.root_of(&path) else {
            return;
        };
        if path == root {
            return;
        }
        let excluded = path.strip_prefix(&root).is_ok_and(|relative| match noted {
            Noted::File => filter.rules.excludes_file(relative),
            Noted::Folder => filter.rules.excludes_folder(relative),
            // Decided once a check tells what it is.
            Noted::Uncertain => false,
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
            tracing::warn!(
                root = %root.display(),
                "The library folder was deleted, moved or unmounted, so it is no longer watched. \
                 Its files stay in the library until the next scan."
            );
            lock(&self.roots).remove(&root);
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

        // Drop work for roots that are no longer watched.
        let roots: Vec<(PathBuf, Arc<RootFilter>)> = lock(&self.roots)
            .iter()
            .map(|(root, filter)| (root.clone(), Arc::clone(filter)))
            .collect();
        self.pending
            .retain(|path, _| roots.iter().any(|(root, _)| path.starts_with(root)));

        let checks: Vec<Check> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.next_check <= now)
            .map(|(path, pending)| Check {
                path: path.clone(),
                previous: pending.observed,
                open_if_unchanged: pending.settled(now, settle),
            })
            .collect();
        if checks.is_empty() {
            return Ok(());
        }
        let findings = tokio::task::spawn_blocking(move || {
            checks
                .into_iter()
                .map(|check| inspect(check, &roots))
                .collect::<Vec<_>>()
        })
        .await;
        match findings {
            Ok(findings) => self.apply(findings).await,
            Err(error) => {
                tracing::warn!(%error, "checking changed files failed");
                Ok(())
            }
        }
    }

    async fn apply(&mut self, findings: Vec<(PathBuf, Finding)>) -> Result<(), Closed> {
        let now = Instant::now();
        let settle = self.settle;
        let mut ready = Vec::new();
        for (path, finding) in findings {
            let verdict = match finding {
                Finding::Missing => {
                    if self.pending.remove(&path).is_some_and(|p| p.uncertain) {
                        self.gone(path, None).await?;
                    }
                    continue;
                }
                Finding::NotAFile => Verdict::GiveUp,
                Finding::Folder(files) => {
                    self.pending.remove(&path);
                    for (file, observed) in files {
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
                    if let Some(pending) = self.pending.remove(&path) {
                        if pending.trouble_since.is_some() {
                            tracing::warn!(
                                path = %path.display(),
                                "A changed file still cannot be read, so the watcher stopped \
                                 waiting for it; the next scan will pick it up."
                            );
                        }
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
        let Some((root, filter)) = self.root_of(path) else {
            return false;
        };
        if path.to_str().is_none() {
            tracing::debug!(path = %path.display(), "skipping a file whose name is not valid UTF-8");
            return false;
        }
        let excluded = path
            .strip_prefix(&root)
            .map_or(true, |relative| filter.rules.excludes_file(relative));
        !excluded && size >= filter.min_size_bytes
    }

    /// The watched root containing `path` (the innermost one if nested).
    fn root_of(&self, path: &Path) -> Option<(PathBuf, Arc<RootFilter>)> {
        lock(&self.roots)
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.as_os_str().len())
            .map(|(root, filter)| (root.clone(), Arc::clone(filter)))
    }

    async fn emit(&self, event: WatchEvent) -> Result<(), Closed> {
        tracing::trace!(?event, "folder change");
        self.out.send(event).await.map_err(|_| Closed)
    }
}

/// Check one pending path (blocking).
fn inspect(check: Check, roots: &[(PathBuf, Arc<RootFilter>)]) -> (PathBuf, Finding) {
    let finding = match std::fs::metadata(&check.path) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Finding::Missing
        }
        Err(error) => Finding::Failed(error.to_string()),
        Ok(metadata) if metadata.is_dir() => Finding::Folder(media_in_folder(&check.path, roots)),
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

/// Media files inside a folder that appeared (blocking). Files created
/// before the new folder's watch was in place would otherwise be missed.
fn media_in_folder(dir: &Path, roots: &[(PathBuf, Arc<RootFilter>)]) -> Vec<(PathBuf, Observed)> {
    let Some((root, filter)) = roots
        .iter()
        .filter(|(root, _)| dir.starts_with(root))
        .max_by_key(|(root, _)| root.as_os_str().len())
    else {
        return Vec::new();
    };
    let relative = |path: &Path| path.strip_prefix(root).map(Path::to_path_buf).ok();
    if relative(dir).is_some_and(|rel| filter.rules.excludes_folder(&rel)) {
        return Vec::new();
    }
    let entries = WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_dir()
                || !relative(entry.path()).is_some_and(|rel| filter.rules.is_ignored_dir(&rel))
        });
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().is_file() || path.to_str().is_none() || !is_media(path) {
            continue;
        }
        if relative(path).is_some_and(|rel| filter.rules.is_ignored(&rel)) {
            continue;
        }
        if let Ok(metadata) = entry.metadata() {
            let observed = Observed {
                size: metadata.len(),
                modified: metadata.modified().ok(),
            };
            found.push((path.to_path_buf(), observed));
        }
    }
    found
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

        let missing = notify::Error::path_not_found();
        let message = describe_notify_error(&missing, Some(Path::new("/nope")));
        assert!(message.contains("does not exist"), "{message}");
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
}

/// Tests against the real kernel watcher.
#[cfg(all(test, target_os = "linux"))]
mod live_tests {
    use super::*;
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
