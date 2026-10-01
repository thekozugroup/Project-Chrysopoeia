//! LibraryService: scanning libraries, applying watch events, re-deciding
//! files after a profile change, and assembling [`Library`] views.
//!
//! A scan walks the folder (blocking, on a blocking thread), compares what it
//! finds with the database by path, size and modification time, probes new and
//! changed files (4 at a time, 60 s each), decides what each needs under the
//! library's profile and stores the result. Scans of the same library never
//! overlap.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use chrysopoeia_core::{
    ActivityLevel, Event, FileStatus, Library, LibraryStats, ProbeInfo, ProblemKind, ScanPhase,
    ScanProgress, TranscodeProfile,
};
use chrysopoeia_scanner::{DiscoveredFile, IgnoreRules, ProbeError, ScanOptions, WatchEvent};
use chrysopoeia_worker::Decision;
use chrysopoeia_worker::finalize::Recovery;
use futures::StreamExt;
use globset::{Glob, GlobSet, GlobSetBuilder};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::activity::ActivityRefs;
use crate::db::files::{FileUpsert, IndexEntry, RemovedRow};
use crate::db::jobs::NewJob;
use crate::db::libraries::LibraryRow;
use crate::db::{self, ts};
use crate::format::{count, plural};
use crate::services::fs_guard;
use crate::services::watcher::ActiveWatcher;
use crate::state::{AppState, lock};

/// Probes running at once across all scans and watch events.
pub const PROBE_CONCURRENCY: usize = 4;
/// ffprobe gets this long per file.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
/// Minimum time between `scan.progress` events of one scan.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);
/// Files written per database transaction during a scan.
const WRITE_BATCH: usize = 50;
/// Rows deleted per transaction when a scan removes files.
const DELETE_CHUNK: usize = 500;
/// Files re-decided per transaction after a profile change.
const REDECIDE_CHUNK: usize = 500;
/// Skip reason for files the user skipped by hand; never re-decided.
pub const USER_SKIP_REASON: &str = "Skipped by you";

/// Scans in progress and the folder watcher.
pub struct LibraryHandle {
    scans: std::sync::Mutex<HashMap<Uuid, CancellationToken>>,
    /// The running folder watcher, when watching is on.
    pub(crate) watcher: std::sync::Mutex<Option<ActiveWatcher>>,
    /// Serializes watcher changes (see `watcher::sync`).
    pub(crate) watcher_sync: tokio::sync::Mutex<()>,
    /// Watcher problems already reported, so the feed says each only once.
    pub(crate) reported_watch_problems: std::sync::Mutex<HashSet<String>>,
    /// Scan problems and notes already reported, per library.
    reported_walk_problems: std::sync::Mutex<HashMap<Uuid, HashSet<String>>>,
    /// Rows of files removed a moment ago, in case they turn up under
    /// another name (see [`remember_removed`]).
    removed: std::sync::Mutex<RecentlyRemoved>,
    probes: Arc<Semaphore>,
    /// Per library, the number of the scan whose files still being copied
    /// are being looked at again (see [`recheck_settling`]); a newer scan
    /// takes over from an older one.
    settle_scans: std::sync::Mutex<HashMap<Uuid, u64>>,
    /// Per library, files still being copied as the last scan and the
    /// folder watcher see them (see [`record_settling`]).
    settling: std::sync::Mutex<HashMap<Uuid, SettlingCounts>>,
}

/// Files of a library still being copied, as two sources see them. The
/// library shows the larger count (both usually see the same files).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SettlingCounts {
    /// Left for later by the last scan, not yet settled.
    scan: u64,
    /// Waited on by the folder watcher right now.
    watch: u64,
}

impl SettlingCounts {
    fn shown(self) -> u64 {
        self.scan.max(self.watch)
    }
}

/// Which count [`record_settling`] updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettlingSource {
    Scan(u64),
    Watch(u64),
}

/// How long a removed file's row is remembered for a rename or move.
const MOVE_WINDOW: Duration = Duration::from_secs(3600);
/// Most removed rows remembered at once.
const MOVE_MEMORY: usize = 50_000;

/// Recently removed rows by (library, size, modification time).
#[derive(Default)]
pub(crate) struct RecentlyRemoved {
    rows: HashMap<(Uuid, u64, String), Vec<(Instant, RemovedRow)>>,
    count: usize,
}

impl Default for LibraryHandle {
    fn default() -> Self {
        Self {
            scans: std::sync::Mutex::new(HashMap::new()),
            watcher: std::sync::Mutex::new(None),
            watcher_sync: tokio::sync::Mutex::new(()),
            reported_watch_problems: std::sync::Mutex::new(HashSet::new()),
            reported_walk_problems: std::sync::Mutex::new(HashMap::new()),
            removed: std::sync::Mutex::new(RecentlyRemoved::default()),
            probes: Arc::new(Semaphore::new(PROBE_CONCURRENCY)),
            settle_scans: std::sync::Mutex::new(HashMap::new()),
            settling: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl LibraryHandle {
    /// Whether a library is being scanned.
    pub fn is_scanning(&self, id: Uuid) -> bool {
        lock(&self.scans).contains_key(&id)
    }

    /// Ids of libraries being scanned.
    pub fn scanning_ids(&self) -> HashSet<Uuid> {
        lock(&self.scans).keys().copied().collect()
    }

    /// Stop a library's scan, if one runs.
    pub fn cancel_scan(&self, id: Uuid) {
        if let Some(token) = lock(&self.scans).get(&id) {
            token.cancel();
        }
    }

    /// Stop every scan (shutdown).
    pub fn cancel_all_scans(&self) {
        for token in lock(&self.scans).values() {
            token.cancel();
        }
    }
}

/// Remember the rows of files that were just removed. A renamed file or
/// folder shows up as a removal plus a new file with the same size and
/// modification time; the new file then keeps the old row's state (a
/// finished conversion, a skip) instead of being probed and converted again.
/// Queued files and files whose last job found them missing are not
/// remembered: they are decided afresh.
fn remember_removed(state: &AppState, rows: Vec<RemovedRow>) {
    let now = Instant::now();
    let mut memory = lock(&state.library.removed);
    if memory.count + rows.len() > MOVE_MEMORY {
        memory.rows.retain(|_, v| {
            v.iter()
                .any(|(at, _)| now.duration_since(*at) < MOVE_WINDOW)
        });
        memory.count = memory.rows.values().map(Vec::len).sum();
        if memory.count + rows.len() > MOVE_MEMORY {
            memory.rows.clear();
            memory.count = 0;
        }
    }
    for row in rows {
        let missing = row
            .error
            .as_deref()
            .is_some_and(|e| e.starts_with(db::files::MISSING_INPUT_ERROR));
        if matches!(row.status, FileStatus::Queued | FileStatus::Processing) || missing {
            continue;
        }
        let key = (row.library_id, row.size_bytes, row.modified_at.clone());
        memory.rows.entry(key).or_default().push((now, row));
        memory.count += 1;
    }
}

/// The remembered row of a file removed from `library` within the last hour
/// with this size and modification time, if there is exactly one.
fn take_moved(
    state: &AppState,
    library: Uuid,
    size: u64,
    modified: DateTime<Utc>,
) -> Option<RemovedRow> {
    let mut memory = lock(&state.library.removed);
    let key = (library, size, ts(modified));
    let entries = memory.rows.get_mut(&key)?;
    entries.retain(|(at, _)| at.elapsed() < MOVE_WINDOW);
    let found = match entries.len() {
        // Two removed files alike: can't tell which one this is.
        1 => entries.pop().map(|(_, row)| row),
        _ => None,
    };
    if entries.is_empty() {
        memory.rows.remove(&key);
    }
    if found.is_some() {
        memory.count = memory.count.saturating_sub(1);
    }
    found
}

/// Store a file found under a new name with the state of its removed row.
/// Returns the new row's id (`None` when the path has a row already).
async fn store_moved(
    state: &AppState,
    lib: &LibraryRow,
    file: &DiscoveredFile,
    from: &RemovedRow,
) -> sqlx::Result<Option<Uuid>> {
    let Some((path, relative_path, file_name)) = row_names(Path::new(&lib.path), file) else {
        return Ok(None);
    };
    // Status, probe and reasons come from `from`.
    let upsert = FileUpsert {
        library_id: lib.id,
        path,
        relative_path,
        file_name,
        size_bytes: file.size,
        modified_at: file.modified,
        status: from.status,
        probe: None,
        skip_reason: None,
        error: None,
        problem: None,
        verdict_profile: None,
    };
    let mut tx = state.db.write_tx().await?;
    let id = db::files::insert_moved(&mut tx, &upsert, from).await?;
    tx.commit().await?;
    if id.is_some() {
        tracing::debug!(path = %file.path.display(), "a moved or renamed file kept its state");
    }
    Ok(id)
}

/// Removes a scan from the in-progress set when dropped, even on panic.
struct ScanGuard {
    state: AppState,
    id: Uuid,
}

impl Drop for ScanGuard {
    fn drop(&mut self) {
        lock(&self.state.library.scans).remove(&self.id);
    }
}

/// Why a scan could not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStartError {
    /// A scan of this library is already running.
    AlreadyScanning,
}

/// Start scanning a library in the background.
pub fn start_scan(state: &AppState, id: Uuid) -> Result<(), ScanStartError> {
    let token = state.shutdown.child_token();
    {
        let mut scans = lock(&state.library.scans);
        if scans.contains_key(&id) {
            return Err(ScanStartError::AlreadyScanning);
        }
        scans.insert(id, token.clone());
    }
    let guard = ScanGuard {
        state: state.clone(),
        id,
    };
    let state = state.clone();
    tokio::spawn(async move {
        state.broadcast_library(id).await;
        let result = run_scan(&state, id, &token).await;
        drop(guard);
        if let Err(e) = result {
            if token.is_cancelled() {
                tracing::debug!(library = %id, "scan stopped: {e}");
            } else {
                tracing::error!(library = %id, "scan failed: {e:#}");
                state
                    .library_activity(
                        ActivityLevel::Error,
                        "A library scan stopped because of a database problem. The details are in the server log.",
                        id,
                    )
                    .await;
            }
        }
        state.broadcast_library(id).await;
    });
    Ok(())
}

/// Scan every enabled library that is not already being scanned. Returns
/// how many scans started.
pub async fn scan_all(state: &AppState) -> sqlx::Result<usize> {
    let mut started = 0;
    for lib in db::libraries::list(state.db.pool()).await? {
        if lib.enabled && start_scan(state, lib.id).is_ok() {
            started += 1;
        }
    }
    Ok(started)
}

/// Options for walking a library under the current settings (the same the
/// folder watcher filters with).
fn scan_options(state: &AppState) -> ScanOptions {
    ScanOptions::from_settings(&state.settings())
}

/// `text` as a sentence: trimmed, first letter capitalized, ending with a
/// full stop (or another closing punctuation mark).
fn sentence(text: &str) -> String {
    let text = text.trim();
    let mut chars = text.chars();
    let mut out: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => return String::new(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

/// Why a library folder couldn't be walked, as one or more sentences. The
/// scanner's reasons are already sentences with advice; anything else gets
/// the usual advice added.
fn scan_failure(e: &anyhow::Error) -> String {
    let text = sentence(&format!("{e:#}"));
    if text.contains("Check ") || text.contains("check ") {
        text
    } else {
        format!("{text} Check that the folder exists and its drive or share is connected.")
    }
}

/// Plain-language message for a probe failure. The scanner's reasons for
/// unreadable files are already sentences for users.
pub fn probe_error_message(e: &ProbeError) -> String {
    match e {
        // The reason may start with a program name ("ffprobe was not
        // found …"), which keeps its lower case.
        ProbeError::Spawn(msg) => {
            let msg = msg.trim();
            let end = if msg.ends_with(['.', '!', '?']) {
                ""
            } else {
                "."
            };
            format!("ffprobe couldn't be started, so this file wasn't checked. {msg}{end}")
        }
        ProbeError::Timeout(_) => "Reading this file took longer than 60 seconds. It may be \
            damaged, or the drive may be very slow."
            .to_string(),
        ProbeError::Unreadable(msg) => sentence(msg),
        ProbeError::Parse(msg) => {
            tracing::debug!("ffprobe's answer could not be read: {msg}");
            "Chrysopoeia couldn't make sense of what ffprobe said about this file, so it wasn't \
             checked. Scan the library again; if it keeps happening, the file may be damaged."
                .to_string()
        }
    }
}

/// What kind of problem a probe failure is: a file that can't be read (or
/// takes too long to) is the file's problem; ffprobe not starting or
/// answering nonsense is not.
pub fn probe_problem(e: &ProbeError) -> ProblemKind {
    match e {
        ProbeError::Unreadable(_) | ProbeError::Timeout(_) => ProblemKind::UnreadableSource,
        ProbeError::Spawn(_) | ProbeError::Parse(_) => ProblemKind::Other,
    }
}

/// What a probed file needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Transcode,
    Skip(String),
    /// It can't be converted: why, and what kind of problem that is.
    Broken(ProblemKind, String),
}

/// Decide what a file needs, turning tool failures into a `Broken` verdict.
pub fn verdict(
    state: &AppState,
    probe: &Result<ProbeInfo, ProbeError>,
    profile: &TranscodeProfile,
) -> Verdict {
    match probe {
        Err(e) => Verdict::Broken(probe_problem(e), probe_error_message(e)),
        Ok(p) => match state.toolkit.decide(p, profile) {
            Ok(Decision::Transcode) => Verdict::Transcode,
            Ok(Decision::Skip { reason }) => Verdict::Skip(reason),
            Err(_) => Verdict::Broken(
                ProblemKind::Other,
                "Chrysopoeia couldn't work out what this file needs. The details are in the \
                 server log."
                    .to_string(),
            ),
        },
    }
}

/// One analyzed file, ready to be written.
struct Analyzed {
    /// The stored row as the scan read it (the write is skipped when the row
    /// changed since), or `None` for a new file.
    existing: Option<IndexEntry>,
    upsert: FileUpsert,
}

/// A file's path, path relative to the library root, and name, as stored.
/// `None` when the path isn't valid UTF-8.
fn row_names(root: &Path, file: &DiscoveredFile) -> Option<(String, String, String)> {
    let path = file.path.to_str()?.to_string();
    let relative_path = file
        .path
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.clone());
    let file_name = file
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    Some((path, relative_path, file_name))
}

/// The row to store for a probed and decided file. `goal` is the stored
/// form of the goal the verdict was decided with
/// (`db::files::profile_json`).
fn build_upsert(
    library_id: Uuid,
    root: &Path,
    file: &DiscoveredFile,
    probe: Result<ProbeInfo, ProbeError>,
    verdict: Verdict,
    auto_queue: bool,
    goal: &str,
) -> Option<FileUpsert> {
    let (path, relative_path, file_name) = row_names(root, file)?;
    let verdict_profile = (!matches!(verdict, Verdict::Broken(..))).then(|| goal.to_string());
    let (status, skip_reason, error, problem) = match verdict {
        Verdict::Transcode if auto_queue => (FileStatus::Queued, None, None, None),
        Verdict::Transcode => (FileStatus::Pending, None, None, None),
        Verdict::Skip(reason) => (FileStatus::Skipped, Some(reason), None, None),
        Verdict::Broken(problem, error) => (FileStatus::Failed, None, Some(error), Some(problem)),
    };
    Some(FileUpsert {
        library_id,
        path,
        relative_path,
        file_name,
        size_bytes: file.size,
        modified_at: file.modified,
        status,
        probe: probe.ok(),
        skip_reason,
        error,
        problem,
        verdict_profile,
    })
}

/// Write analyzed files in one transaction, creating jobs for queued ones.
/// A stored row is only overwritten when nothing wrote it since the scan read
/// it: rows a job finished, a watch event or the user changed meanwhile, and
/// rows that became queued or processing, are left alone. A new file whose row
/// appeared at or after `read_at` (the time the scan read the database) was
/// just written by someone else and is left alone too. Returns the ids
/// written and how many jobs were created.
async fn write_batch(
    state: &AppState,
    batch: Vec<Analyzed>,
    read_at: &str,
) -> sqlx::Result<(Vec<Uuid>, usize)> {
    if batch.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let mut tx = state.db.write_tx().await?;
    let mut ids = Vec::with_capacity(batch.len());
    let mut jobs = 0;
    for item in batch {
        let wants_job = item.upsert.status == FileStatus::Queued;
        let mut row = item.upsert;
        if wants_job {
            // `jobs::create` moves it to queued together with its job.
            row.status = FileStatus::Pending;
        }
        let id = match item.existing {
            Some(seen) => {
                if !db::files::update_scanned(&mut tx, &seen, &row).await? {
                    continue;
                }
                seen.id
            }
            None => match db::files::insert(&mut tx, &row).await? {
                Some(id) => id,
                None => {
                    let Some(current) = db::files::find_by_path_conn(&mut tx, &row.path).await?
                    else {
                        continue;
                    };
                    let written_meanwhile = current.updated_at.as_str() >= read_at;
                    let unchanged = current.size_bytes == row.size_bytes
                        && current.modified_at == ts(row.modified_at);
                    if written_meanwhile
                        || unchanged
                        || !db::files::update_scanned(&mut tx, &current, &row).await?
                    {
                        continue;
                    }
                    current.id
                }
            },
        };
        if wants_job {
            let created = db::jobs::create(
                &mut tx,
                &NewJob {
                    file_id: id,
                    library_id: row.library_id,
                    file_name: &row.file_name,
                    file_path: &row.path,
                    input_size: row.size_bytes,
                    priority: 0,
                    force: false,
                },
            )
            .await?;
            jobs += usize::from(created.is_some());
        }
        ids.push(id);
    }
    tx.commit().await?;
    Ok((ids, jobs))
}

/// Probe one file, holding a slot of the shared probe semaphore.
async fn probe_limited(state: &AppState, path: PathBuf) -> Result<ProbeInfo, ProbeError> {
    let _permit = Arc::clone(&state.library.probes).acquire_owned().await;
    state.toolkit.probe_file(path, PROBE_TIMEOUT).await
}

/// Whether a probe failed only because the file disappeared in the meantime
/// (moved, renamed, or replaced by a conversion), or because it stopped
/// answering (a share whose server went away). Such files are not reported
/// as broken; the next scan sees them as they are.
async fn vanished(probe: &Result<ProbeInfo, ProbeError>, path: &Path) -> bool {
    probe.is_err()
        && matches!(
            fs_guard::symlink_metadata(path, FILE_CHECK_TIMEOUT).await,
            Err(_) | Ok(Err(std::io::ErrorKind::NotFound))
        )
}

/// How long a single file may take to answer a look at it (a watch event,
/// a probe that failed) before it counts as not responding.
const FILE_CHECK_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(5)
} else {
    Duration::from_secs(30)
};

/// Whether a file was modified so recently that it may still be being
/// copied. Modification times far in the future (a clock that is off) don't
/// count, so such files are never held back forever.
pub fn still_settling(modified: DateTime<Utc>, settle: Duration) -> bool {
    if settle.is_zero() {
        return false;
    }
    let age = Utc::now().signed_duration_since(modified);
    let settle = chrono::Duration::from_std(settle).unwrap_or(chrono::Duration::MAX);
    age < settle && age > -chrono::Duration::minutes(1)
}

/// How many times a file that is still being written is looked at again
/// before it is left to the next scan (with the default settle time, about
/// an hour).
const SETTLE_ROUNDS: u32 = 180;

/// Look at files that were still being written again once they had time to
/// settle, so they don't wait for the next scan.
fn recheck_settling(state: &AppState, paths: Vec<PathBuf>) {
    recheck_settling_for(state, paths, None);
}

/// Record how many files a finished scan of `library_id` left for later
/// because they were still being copied (`LibraryStats::settling`), and
/// look at them again as they settle, lowering the count as they are
/// added. A later scan of the library takes over the count.
async fn track_settling(state: &AppState, library_id: Uuid, paths: Vec<PathBuf>) {
    let scan = {
        let mut scans = lock(&state.library.settle_scans);
        let n = scans.entry(library_id).or_insert(0);
        *n += 1;
        *n
    };
    let count = paths.len() as u64;
    if let Err(e) = record_settling(state, library_id, SettlingSource::Scan(count)).await {
        tracing::warn!(library = %library_id, "could not record the files still being copied: {e}");
    }
    recheck_settling_for(state, paths, Some((library_id, scan)));
}

/// Update one source's count of a library's files still being copied and
/// store the count the library shows (`LibraryStats::settling`, kept in
/// the database so a restart rescans libraries that were waiting for
/// copies). A scan's count is always stored (the stored one may be from
/// before a restart). Returns whether the shown count changed.
async fn record_settling(
    state: &AppState,
    library_id: Uuid,
    source: SettlingSource,
) -> sqlx::Result<bool> {
    let (before, after) = {
        let mut all = lock(&state.library.settling);
        let counts = all.entry(library_id).or_default();
        let before = counts.shown();
        match source {
            SettlingSource::Scan(n) => counts.scan = n,
            SettlingSource::Watch(n) => counts.watch = n,
        }
        (before, counts.shown())
    };
    if before != after || matches!(source, SettlingSource::Scan(_)) {
        db::stats::set_settling(state.db.pool(), library_id, after).await?;
    }
    Ok(before != after)
}

/// How long after a conversion finished its result still doesn't count as
/// a copy in progress, on top of the settle time the watcher waits.
const OWN_OUTPUT_GRACE: Duration = Duration::from_secs(30);

/// Files the folder watcher waits on (by watched folder) that are copies
/// still in progress, as counts per folder: without the files Chrysopoeia
/// itself is writing. A converted file put in place goes through the
/// watcher's settle wait like any new file, but it is complete, so it must
/// not make the library say it is waiting for a copy to finish. Those files
/// are the results of the jobs running now and of the jobs that finished
/// within the settle time (plus a margin).
pub async fn settling_copies(
    state: &AppState,
    waiting: &HashMap<PathBuf, Vec<PathBuf>>,
) -> sqlx::Result<HashMap<PathBuf, usize>> {
    if waiting.values().all(Vec::is_empty) {
        return Ok(HashMap::new());
    }
    let window = chrono::Duration::from_std(state.config.settle + OWN_OUTPUT_GRACE)
        .unwrap_or(chrono::Duration::MAX);
    let since = Utc::now()
        .checked_sub_signed(window)
        .unwrap_or(DateTime::<Utc>::MIN_UTC);
    let own = db::jobs::own_outputs(state.db.pool(), since).await?;
    Ok(waiting
        .iter()
        .filter_map(|(root, files)| {
            let copies = files
                .iter()
                .filter(|path| !path.to_str().is_some_and(|p| own.contains(p)))
                .count();
            (copies > 0).then(|| (root.clone(), copies))
        })
        .collect())
}

/// The folder watcher's count of media files still being written, per
/// watched folder: store it as the libraries' settling count where it
/// changed, and tell the UI.
pub async fn set_watch_settling(
    state: &AppState,
    waiting: &HashMap<PathBuf, usize>,
) -> sqlx::Result<()> {
    let libs = db::libraries::list(state.db.pool()).await?;
    lock(&state.library.settling).retain(|id, _| libs.iter().any(|l| l.id == *id));
    let mut changed = false;
    for lib in &libs {
        let n = waiting.get(Path::new(&lib.path)).copied().unwrap_or(0) as u64;
        if record_settling(state, lib.id, SettlingSource::Watch(n)).await? {
            state.broadcast_library(lib.id).await;
            changed = true;
        }
    }
    if changed {
        state.broadcast_stats().await;
    }
    Ok(())
}

/// [`recheck_settling`], keeping the settling count of the scan `owner`
/// (library, scan number) up to date while that scan is the library's last.
fn recheck_settling_for(state: &AppState, paths: Vec<PathBuf>, owner: Option<(Uuid, u64)>) {
    if paths.is_empty() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let wait = state.config.settle.max(Duration::from_secs(1));
        let mut pending = paths;
        for _ in 0..SETTLE_ROUNDS {
            tokio::select! {
                () = state.shutdown.cancelled() => return,
                () = tokio::time::sleep(wait) => {}
            }
            let mut still = Vec::new();
            for path in pending {
                match upsert_single(&state, &path).await {
                    Ok(Single::Settling) => still.push(path),
                    Ok(Single::Handled) => {}
                    Err(e) => {
                        tracing::warn!(path = %path.display(), "could not check a new file: {e:#}");
                    }
                }
            }
            if let Some((library_id, scan)) = owner {
                let current = lock(&state.library.settle_scans).get(&library_id).copied();
                if current != Some(scan) {
                    // A newer scan found these files again and looks after them.
                    return;
                }
                let count = still.len() as u64;
                match record_settling(&state, library_id, SettlingSource::Scan(count)).await {
                    Ok(_) => {
                        state.broadcast_library(library_id).await;
                        state.broadcast_stats().await;
                    }
                    Err(e) => tracing::warn!(
                        library = %library_id,
                        "could not record the files still being copied: {e}"
                    ),
                }
            }
            if still.is_empty() {
                return;
            }
            pending = still;
        }
    });
}

/// A file a scan probed, waiting to be decided and stored.
struct Probed {
    existing: Option<IndexEntry>,
    file: DiscoveredFile,
    probe: Result<ProbeInfo, ProbeError>,
}

/// What [`flush`] did.
struct Flushed {
    broken: u64,
    jobs: usize,
}

/// Decide a batch of probed files with the library's current profile and
/// store it (with that profile as the goal of each verdict).
async fn flush(
    state: &AppState,
    lib: &LibraryRow,
    batch: Vec<Probed>,
    read_at: &str,
) -> sqlx::Result<Flushed> {
    let profile = db::libraries::get(state.db.pool(), lib.id)
        .await?
        .map_or_else(|| lib.profile.clone(), |l| l.profile);
    let goal = db::files::profile_json(&profile)?;
    let auto_queue = state.settings().auto_queue;
    let root = Path::new(&lib.path);
    let mut broken = 0;
    let mut analyzed = Vec::with_capacity(batch.len());
    for p in batch {
        let v = verdict(state, &p.probe, &profile);
        if matches!(v, Verdict::Broken(..)) {
            broken += 1;
        }
        if let Some(upsert) = build_upsert(lib.id, root, &p.file, p.probe, v, auto_queue, &goal) {
            analyzed.push(Analyzed {
                existing: p.existing,
                upsert,
            });
        }
    }
    let (_, jobs) = write_batch(state, analyzed, read_at).await?;
    Ok(Flushed { broken, jobs })
}

/// How long a library folder may take to answer before it is reported as
/// not responding (a hung network share must not hang the API).
const PATH_CHECK_TIMEOUT: Duration = Duration::from_secs(3);

fn scan_progress(
    lib: &LibraryRow,
    phase: ScanPhase,
    discovered: u64,
    analyzed: u64,
    to_analyze: u64,
) -> Event {
    Event::ScanProgress(ScanProgress {
        library_id: lib.id,
        library_name: lib.name.clone(),
        phase,
        discovered,
        analyzed,
        to_analyze,
    })
}

/// Paths a walk couldn't read or left alone, as the feed shows them:
/// relative to the library, at most [`NAMED_PROBLEMS`] named, then a count.
fn describe_problems(root: &Path, items: &[&(PathBuf, String)]) -> String {
    let mut parts: Vec<String> = items
        .iter()
        .take(NAMED_PROBLEMS)
        .map(|(path, reason)| {
            let reason = reason.trim().trim_end_matches('.');
            // A note on the library folder itself (an unusable ignore
            // pattern) needs no path.
            if path == root {
                return reason.to_string();
            }
            let shown = path
                .strip_prefix(root)
                .ok()
                .filter(|r| !r.as_os_str().is_empty())
                .unwrap_or(path);
            format!("{}: {reason}", shown.to_string_lossy())
        })
        .collect();
    if items.len() > NAMED_PROBLEMS {
        parts.push(format!("and {} more", items.len() - NAMED_PROBLEMS));
    }
    format!("{}.", parts.join("; "))
}

/// Tell the user, once per server run, about folders a scan couldn't read
/// (a warning) and about what it deliberately left alone, such as disc
/// copies and links (an informational note). Details go to the debug log.
async fn report_walk_problems(
    state: &AppState,
    lib: &LibraryRow,
    errors: &[(PathBuf, String)],
    notes: &[(PathBuf, String)],
) {
    for (path, reason) in errors {
        tracing::debug!(library = %lib.name, path = %path.display(), "couldn't read: {reason}");
    }
    for (path, note) in notes {
        tracing::debug!(library = %lib.name, path = %path.display(), "left alone: {note}");
    }
    let (new_errors, new_notes) = {
        let mut reported = lock(&state.library.reported_walk_problems);
        let seen = reported.entry(lib.id).or_default();
        let mut fresh = |items: &'_ [(PathBuf, String)]| -> Vec<(PathBuf, String)> {
            items
                .iter()
                .filter(|(p, r)| seen.insert(format!("{}\n{r}", p.display())))
                .cloned()
                .collect()
        };
        (fresh(errors), fresh(notes))
    };
    let root = Path::new(&lib.path);
    if !new_errors.is_empty() {
        let items: Vec<&(PathBuf, String)> = new_errors.iter().collect();
        state
            .library_activity(
                ActivityLevel::Warning,
                format!(
                    "Some of {} couldn't be read, so it was left out of the scan. {}",
                    lib.name,
                    describe_problems(root, &items)
                ),
                lib.id,
            )
            .await;
    }
    if !new_notes.is_empty() {
        let items: Vec<&(PathBuf, String)> = new_notes.iter().collect();
        state
            .library_activity(
                ActivityLevel::Info,
                format!(
                    "Left alone in {}: {}",
                    lib.name,
                    describe_problems(root, &items)
                ),
                lib.id,
            )
            .await;
    }
}

/// How many paths a scan problem message names one by one.
const NAMED_PROBLEMS: usize = 3;

/// Whether `path` is inside one of the folders that could not be read.
fn under_unreadable(path: &str, unreadable: &[PathBuf]) -> bool {
    let p = Path::new(path);
    unreadable.iter().any(|dir| p.starts_with(dir))
}

async fn run_scan(state: &AppState, id: Uuid, cancel: &CancellationToken) -> anyhow::Result<()> {
    let Some(lib) = db::libraries::get(state.db.pool(), id).await? else {
        return Ok(());
    };
    let started = Instant::now();
    state.emit(scan_progress(&lib, ScanPhase::Discovering, 0, 0, 0));

    // Read the database before walking. A job that finishes during a long
    // walk then shows up as a row that changed since this snapshot (and is
    // left alone) instead of looking like a removed or changed file.
    let read_at = db::now_ts();
    let index: HashMap<String, IndexEntry> = db::files::index(state.db.pool(), id)
        .await?
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();

    // The minimum size only decides which files are added: the walk lists
    // every size, so a file already in the list (typically a conversion
    // that came out smaller than the minimum) is never taken for removed.
    let opts = scan_options(state);
    let min_size = opts.min_size_bytes;
    let walk_opts = ScanOptions {
        min_size_bytes: 0,
        ..opts
    };
    let walk = match state
        .toolkit
        .walk_library(PathBuf::from(&lib.path), walk_opts)
        .await
    {
        Ok(Ok(walk)) => walk,
        Ok(Err(e)) => {
            state
                .library_activity(
                    ActivityLevel::Error,
                    format!("Couldn't scan {}. {}", lib.name, scan_failure(&e)),
                    id,
                )
                .await;
            state.emit(scan_progress(&lib, ScanPhase::Done, 0, 0, 0));
            return Ok(());
        }
        Err(panic) => {
            state
                .library_activity(
                    ActivityLevel::Error,
                    format!("Couldn't scan {}: {panic}.", lib.name),
                    id,
                )
                .await;
            state.emit(scan_progress(&lib, ScanPhase::Done, 0, 0, 0));
            return Ok(());
        }
    };
    if cancel.is_cancelled() {
        return Ok(());
    }
    // The folder answered, so conversions waiting for it can go on.
    if !walk.files.is_empty() {
        state.dispatcher.library_back(id);
    }
    // Files below the minimum size that aren't listed yet are left out.
    let small_new = |file: &DiscoveredFile| {
        file.size < min_size && file.path.to_str().is_none_or(|p| !index.contains_key(p))
    };
    let discovered = walk.files.iter().filter(|f| !small_new(f)).count() as u64;
    state.emit(scan_progress(
        &lib,
        ScanPhase::Discovering,
        discovered,
        0,
        0,
    ));
    report_walk_problems(state, &lib, &walk.errors, &walk.notes).await;

    // Leftovers of interrupted conversions (a temp file left behind when a
    // folder was renamed during a conversion, an original moved aside by a
    // crash): cleaned up, and an original put back is not taken for removed.
    // Originals a job may still put a new file in place of are not gone
    // either (see `recover_leftovers`).
    let restored: HashSet<String> = if walk.artifacts.is_empty() {
        HashSet::new()
    } else {
        recover_leftovers(state, walk.artifacts.clone(), None)
            .await
            .not_gone()
            .filter_map(|p| p.to_str().map(str::to_string))
            .collect()
    };
    if !walk.files.is_empty() || !walk.artifacts.is_empty() || index.is_empty() {
        lock(&state.leftovers)
            .unreached
            .remove(Path::new(&lib.path));
    }

    let mut seen: HashSet<&str> = HashSet::with_capacity(walk.files.len());
    let mut to_analyze: Vec<(Option<IndexEntry>, DiscoveredFile)> = Vec::new();
    let mut settling: Vec<PathBuf> = Vec::new();
    let mut non_utf8 = 0usize;
    for file in &walk.files {
        let Some(path) = file.path.to_str() else {
            non_utf8 += 1;
            continue;
        };
        seen.insert(path);
        let existing = index.get(path);
        if existing.is_none() && file.size < min_size {
            continue;
        }
        match existing {
            Some(e) if matches!(e.status, FileStatus::Queued | FileStatus::Processing) => continue,
            Some(e)
                if e.size_bytes == file.size
                    && e.modified_at == ts(file.modified)
                    && !e.missing_input =>
            {
                continue;
            }
            _ => {}
        }
        if still_settling(file.modified, state.config.settle) {
            settling.push(file.path.clone());
            continue;
        }
        to_analyze.push((existing.cloned(), file.clone()));
    }
    if non_utf8 > 0 {
        tracing::warn!(
            library = %lib.name,
            "{non_utf8} files have names that aren't valid UTF-8 and were left out"
        );
    }

    // Drives and shares mounted inside the library: one that is no longer
    // mounted left an empty folder behind, and its files are kept.
    let offline_mounts = mounts::check(state, &lib).await;
    if !offline_mounts.is_empty() {
        let problems: Vec<(PathBuf, String)> = offline_mounts
            .iter()
            .map(|p| (p.clone(), mounts::OFFLINE_REASON.to_string()))
            .collect();
        report_walk_problems(state, &lib, &problems, &[]).await;
    }

    // Files that disappeared. An empty walk of a library that had files is
    // almost always an unmounted drive, so nothing is removed then. Nothing
    // below a folder that couldn't be read (or was left alone) was listed,
    // so its files are kept too, as are those of a drive mounted inside the
    // library that isn't mounted now. Notes about the library folder itself
    // (an ignore pattern that can't be used) don't hide anything.
    let root = Path::new(&lib.path);
    let unreadable: Vec<PathBuf> = walk
        .errors
        .iter()
        .chain(walk.notes.iter().filter(|(p, _)| p != root))
        .map(|(p, _)| p.clone())
        .chain(offline_mounts)
        .collect();
    // A file whose original may be moved aside right now by a conversion
    // whose new file is still being put in place, or settled after a stop
    // (see `dispatcher::settle`), is not gone: its job records what became
    // of it. Read after the walk; once the mark is cleared, the record that
    // cleared it changed the file's row, which a removal leaves alone.
    let held: HashSet<Uuid> = match db::jobs::placing(state.db.pool(), None).await {
        Ok(marked) => marked.into_iter().map(|m| m.job.file_id).collect(),
        Err(e) => {
            tracing::warn!(library = %lib.name, "could not look for conversions being put in place: {e}");
            index.values().map(|e| e.id).collect()
        }
    };
    let removed: Vec<IndexEntry> = index
        .values()
        .filter(|e| !seen.contains(e.path.as_str()) && !restored.contains(&e.path))
        .filter(|e| e.status != FileStatus::Processing && !held.contains(&e.id))
        .filter(|e| !under_unreadable(&e.path, &unreadable))
        .cloned()
        .collect();
    let mut removed_count = 0u64;
    if walk.files.is_empty() && !index.is_empty() {
        state
            .library_activity(
                ActivityLevel::Warning,
                format!(
                    "{} looks empty. If its drive or share is disconnected, reconnect it; the \
                     file list was kept.",
                    lib.name
                ),
                id,
            )
            .await;
    } else {
        for chunk in removed.chunks(DELETE_CHUNK) {
            let mut tx = state.db.write_tx().await?;
            let rows = db::files::delete_unchanged(&mut tx, chunk).await?;
            tx.commit().await?;
            removed_count += rows.len() as u64;
            remember_removed(state, rows);
        }
    }

    // New files that are removed ones under another name (a renamed file or
    // folder) keep their state instead of being probed and decided again.
    let mut moved = 0u64;
    let mut still_new = Vec::with_capacity(to_analyze.len());
    for (existing, file) in to_analyze {
        if existing.is_none()
            && let Some(from) = take_moved(state, id, file.size, file.modified)
            && store_moved(state, &lib, &file, &from).await?.is_some()
        {
            moved += 1;
            continue;
        }
        still_new.push((existing, file));
    }
    let to_analyze = still_new;
    removed_count = removed_count.saturating_sub(moved);

    // Probe new and changed files, then decide and store them in batches.
    // Each batch is decided with the profile read right before it is
    // written, so a goal changed during a long scan applies to the rest.
    let total = to_analyze.len() as u64;
    state.emit(scan_progress(
        &lib,
        ScanPhase::Analyzing,
        discovered,
        0,
        total,
    ));
    let mut analyzed = 0u64;
    let mut broken = 0u64;
    let mut jobs_created = 0usize;
    let mut last_progress = Instant::now();
    let mut batch: Vec<Probed> = Vec::with_capacity(WRITE_BATCH);
    let mut results = futures::stream::iter(to_analyze)
        .map(|(existing, file)| {
            let state = state.clone();
            async move {
                let probe = probe_limited(&state, file.path.clone()).await;
                let gone = vanished(&probe, &file.path).await;
                (
                    Probed {
                        existing,
                        file,
                        probe,
                    },
                    gone,
                )
            }
        })
        .buffer_unordered(PROBE_CONCURRENCY);
    while let Some((probed, gone)) = results.next().await {
        if cancel.is_cancelled() {
            return Ok(());
        }
        analyzed += 1;
        if !gone {
            batch.push(probed);
        }
        if batch.len() >= WRITE_BATCH {
            let flushed = flush(state, &lib, std::mem::take(&mut batch), &read_at).await?;
            broken += flushed.broken;
            jobs_created += flushed.jobs;
            // Big libraries: start converting while the scan goes on.
            if flushed.jobs > 0 {
                state.dispatcher.wake();
            }
        }
        if last_progress.elapsed() >= PROGRESS_INTERVAL {
            last_progress = Instant::now();
            state.emit(scan_progress(
                &lib,
                ScanPhase::Analyzing,
                discovered,
                analyzed,
                total,
            ));
        }
    }
    drop(results);
    if !batch.is_empty() {
        let flushed = flush(state, &lib, batch, &read_at).await?;
        broken += flushed.broken;
        jobs_created += flushed.jobs;
    }
    if cancel.is_cancelled() {
        return Ok(());
    }

    // Verdicts decided with another goal than the library's current one:
    // batches decided before the goal changed during this scan (the change
    // itself re-decided only the files stored by then), and any verdict an
    // earlier goal left (or whose goal isn't known).
    let mut redecided = 0;
    if let Some(current) = db::libraries::get(state.db.pool(), id).await? {
        redecided = redecide(state, &current, None).await?;
    }

    db::libraries::set_last_scan(state.db.pool(), id, Utc::now()).await?;
    let stats = db::stats::for_library(state.db.pool(), id).await?;
    let need = stats.pending + stats.queued + stats.processing;
    let mut message = format!(
        "Scanned {}: {}, {} need converting",
        lib.name,
        plural(stats.file_count, "file", "files"),
        count(need)
    );
    if broken > 0 {
        message.push_str(&format!(", {} couldn't be read", count(broken)));
    }
    if removed_count > 0 {
        message.push_str(&format!(", {} removed", count(removed_count)));
    }
    if moved > 0 {
        message.push_str(&format!(", {} moved or renamed", count(moved)));
    }
    if !settling.is_empty() {
        message.push_str(&format!(
            ", {} still being copied (checked again when {} finished)",
            count(settling.len() as u64),
            if settling.len() == 1 {
                "it's"
            } else {
                "they're"
            }
        ));
    }
    tracing::info!(
        library = %lib.name,
        discovered,
        analyzed,
        removed = removed_count,
        settling = settling.len(),
        redecided,
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "scan finished"
    );
    track_settling(state, id, settling).await;
    state.emit(scan_progress(
        &lib,
        ScanPhase::Done,
        discovered,
        analyzed,
        total,
    ));
    state.emit(Event::FilesChanged {
        library_id: Some(id),
    });
    state
        .library_activity(ActivityLevel::Info, message, id)
        .await;
    state.broadcast_stats().await;
    if jobs_created > 0 || redecided > 0 {
        state.dispatcher.wake();
    }
    state.broadcast_queue_state().await;
    Ok(())
}

/// Compile ignore patterns; invalid ones are skipped (settings validation
/// rejects them, so this only matters for hand-edited databases).
pub fn compile_ignore(patterns: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        if let Ok(g) = Glob::new(p) {
            builder.add(g);
        }
    }
    builder.build().unwrap_or_else(|_| GlobSet::empty())
}

/// The enabled library containing `path` (the deepest one).
async fn library_for_path(state: &AppState, path: &Path) -> sqlx::Result<Option<LibraryRow>> {
    let libs = db::libraries::list(state.db.pool()).await?;
    Ok(libs
        .into_iter()
        .filter(|l| l.enabled && path.starts_with(&l.path))
        .max_by_key(|l| l.path.len()))
}

/// Apply one settled watch event.
pub async fn handle_watch_event(state: &AppState, event: WatchEvent) -> anyhow::Result<()> {
    match event {
        WatchEvent::Upserted(path) => {
            if upsert_single(state, &path).await? == Single::Settling {
                recheck_settling(state, vec![path]);
            }
            Ok(())
        }
        WatchEvent::Removed(path) => {
            let Some(p) = path.to_str() else {
                return Ok(());
            };
            let rows = db::files::delete_path_or_prefix(state.db.pool(), p).await?;
            let mut libs: Vec<Uuid> = rows.iter().map(|r| r.library_id).collect();
            libs.sort_unstable();
            libs.dedup();
            remember_removed(state, rows);
            for lib in &libs {
                state.emit(Event::FilesChanged {
                    library_id: Some(*lib),
                });
                state.broadcast_library(*lib).await;
            }
            if !libs.is_empty() {
                state.broadcast_stats().await;
                state.broadcast_queue_state().await;
            }
            Ok(())
        }
    }
}

/// What [`upsert_single`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Single {
    /// Stored, or nothing to do.
    Handled,
    /// The file is still being written; look again later.
    Settling,
}

/// The single-file version of a scan, for a watch event.
async fn upsert_single(state: &AppState, path: &Path) -> anyhow::Result<Single> {
    let Some(path_str) = path.to_str() else {
        return Ok(Single::Handled);
    };
    let Some(lib) = library_for_path(state, path).await? else {
        return Ok(Single::Handled);
    };
    let settings = state.settings();
    let opts = ScanOptions::from_settings(&settings);
    let root = PathBuf::from(&lib.path);
    // The same filters as a scan: the scanner's ignore rules (a pattern that
    // matches a folder excludes everything in it) and the minimum size.
    if let Ok(rel) = path.strip_prefix(&root)
        && IgnoreRules::new(&opts.ignore_patterns).excludes_file(rel)
    {
        return Ok(Single::Handled);
    }
    // A result a conversion is still putting in place: the job records it
    // itself when it ends, so it is neither probed nor added as a new file
    // here (under a new name it has no row yet).
    if db::jobs::is_running_output(state.db.pool(), path_str).await? {
        return Ok(Single::Handled);
    }
    // A file on a share that stopped answering is left for later (one
    // stuck thread at most, however often it is asked about).
    let Ok(Ok(meta)) = fs_guard::metadata(path, FILE_CHECK_TIMEOUT).await else {
        return Ok(Single::Handled);
    };
    if !meta.is_file() {
        return Ok(Single::Handled);
    }
    let modified: DateTime<Utc> = meta
        .modified()
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|_| Utc::now());
    let read_at = db::now_ts();
    let existing = db::files::find_by_path(state.db.pool(), path_str).await?;
    // The minimum size only decides which files are added (see `run_scan`).
    if existing.is_none() && meta.len() < opts.min_size_bytes {
        return Ok(Single::Handled);
    }
    if let Some(e) = &existing {
        let busy = matches!(e.status, FileStatus::Queued | FileStatus::Processing);
        let unchanged =
            e.size_bytes == meta.len() && e.modified_at == ts(modified) && !e.missing_input;
        if busy || unchanged {
            return Ok(Single::Handled);
        }
    }
    if still_settling(modified, state.config.settle) {
        return Ok(Single::Settling);
    }
    let file = DiscoveredFile {
        path: path.to_path_buf(),
        size: meta.len(),
        modified,
    };
    // A file removed under another name moments ago (a rename or move)
    // keeps its state.
    if existing.is_none()
        && let Some(from) = take_moved(state, lib.id, file.size, file.modified)
        && let Some(id) = store_moved(state, &lib, &file, &from).await?
    {
        state.broadcast_file(id).await;
        state.broadcast_library(lib.id).await;
        state.broadcast_stats().await;
        return Ok(Single::Handled);
    }
    let probe = probe_limited(state, file.path.clone()).await;
    if vanished(&probe, &file.path).await {
        return Ok(Single::Handled);
    }
    let v = verdict(state, &probe, &lib.profile);
    let goal = db::files::profile_json(&lib.profile)?;
    let Some(upsert) = build_upsert(lib.id, &root, &file, probe, v, settings.auto_queue, &goal)
    else {
        return Ok(Single::Handled);
    };
    let file_name = upsert.file_name.clone();
    let is_new = existing.is_none();
    let (ids, jobs) = write_batch(state, vec![Analyzed { existing, upsert }], &read_at).await?;
    for id in &ids {
        state.broadcast_file(*id).await;
    }
    if is_new && !ids.is_empty() {
        let refs = ActivityRefs {
            file_id: ids.first().copied(),
            library_id: Some(lib.id),
            job_id: None,
        };
        state
            .activity(
                ActivityLevel::Info,
                format!("Found {file_name} in {}", lib.name),
                refs,
            )
            .await;
    }
    state.broadcast_library(lib.id).await;
    state.broadcast_stats().await;
    if jobs > 0 {
        state.dispatcher.wake();
    }
    state.broadcast_queue_state().await;
    Ok(Single::Handled)
}

/// Profile fields that change how big a conversion's result is. A file a job
/// skipped because the result was not small enough is only worth trying
/// again when one of these changed.
pub(crate) fn affects_output_size(old: &TranscodeProfile, new: &TranscodeProfile) -> bool {
    old.video_codec != new.video_codec
        || old.audio_codec != new.audio_codec
        || old.quality != new.quality
        || old.quality_override != new.quality_override
        || old.speed != new.speed
        || old.max_height != new.max_height
        || old.min_savings_pct != new.min_savings_pct
        || old.audio_languages != new.audio_languages
}

/// Re-decide the `pending` and `skipped` files of a library whose verdict
/// was decided with another goal than its current one (`lib.profile`), or
/// with a goal that isn't known: after a goal change, and at the end of
/// every scan (a verdict a job, a watch event or a scan wrote under an
/// earlier goal). Files the user skipped stay skipped.
///
/// A file a job skipped as not worth keeping (the size rule) stays skipped
/// unless the change affects the output size, judged from the goal its
/// verdict was decided with, else from `previous` (the goal before a
/// change); with neither, it is decided again. Pending files stay pending
/// when the goal would convert them (the user may have taken them out of
/// the queue on purpose); skipped ones are queued (or pending, without
/// auto-queue). Every file looked at records the current goal as its
/// verdict's.
///
/// Works in chunks of [`REDECIDE_CHUNK`] files, each read and written in one
/// short transaction, and stops when the goal changes again meanwhile (that
/// change re-decides the files itself). Returns how many files changed
/// status.
pub async fn redecide(
    state: &AppState,
    lib: &LibraryRow,
    previous: Option<&TranscodeProfile>,
) -> anyhow::Result<u64> {
    let goal = db::files::profile_json(&lib.profile)?;
    let decided_with_current =
        |stored: Option<&str>| db::files::parse_profile(stored).is_some_and(|p| p == lib.profile);
    let stale: Vec<Option<String>> =
        db::files::verdict_profiles(state.db.pool(), lib.id, USER_SKIP_REASON)
            .await?
            .into_iter()
            .filter(|v| !decided_with_current(v.as_deref()))
            .collect();
    if stale.is_empty() {
        return Ok(0);
    }
    let ids = db::files::redecide_candidate_ids(state.db.pool(), lib.id, USER_SKIP_REASON, &stale)
        .await?;
    let auto_queue = state.settings().auto_queue;
    let mut changed = 0u64;
    let mut jobs = 0u64;
    let mut decider_broken = false;
    for chunk in ids.chunks(REDECIDE_CHUNK) {
        let mut tx = state.db.write_tx().await?;
        if db::libraries::profile_conn(&mut tx, lib.id).await?.as_ref() != Some(&lib.profile) {
            tx.rollback().await?;
            break;
        }
        let candidates = db::files::redecide_candidates(&mut tx, chunk, USER_SKIP_REASON).await?;
        let mut to_queue = Vec::new();
        for c in candidates {
            let decided_with = db::files::parse_profile(c.verdict_profile.as_deref());
            // Decided with the current goal meanwhile (a job that ended).
            if decided_with.as_ref() == Some(&lib.profile) {
                continue;
            }
            if c.size_rule_skip
                && decided_with
                    .as_ref()
                    .or(previous)
                    .is_some_and(|before| !affects_output_size(before, &lib.profile))
            {
                db::files::confirm_verdict(&mut tx, c.id, &goal).await?;
                continue;
            }
            let (status, reason) = match state.toolkit.decide(&c.probe, &lib.profile) {
                // Pending files stay pending: the user may have taken them out
                // of the queue on purpose.
                Ok(Decision::Transcode) if auto_queue && c.status == FileStatus::Skipped => {
                    (FileStatus::Queued, None)
                }
                Ok(Decision::Transcode) => (FileStatus::Pending, None),
                Ok(Decision::Skip { reason }) => (FileStatus::Skipped, Some(reason)),
                // A broken decision tool would give the same answer for every
                // file; stop rather than log it thousands of times.
                Err(_) => {
                    decider_broken = true;
                    break;
                }
            };
            if status == FileStatus::Queued {
                to_queue.push(c.id);
            } else if (status, reason.as_deref()) != (c.status, c.skip_reason.as_deref()) {
                db::files::set_status_where(
                    &mut tx,
                    c.id,
                    status,
                    reason.as_deref(),
                    Some(&goal),
                    &[FileStatus::Pending, FileStatus::Skipped],
                )
                .await?;
                if status != c.status {
                    changed += 1;
                }
            } else {
                db::files::confirm_verdict(&mut tx, c.id, &goal).await?;
            }
        }
        let queued = db::jobs::create_many(&mut tx, &to_queue, &[FileStatus::Skipped]).await?;
        tx.commit().await?;
        jobs += queued;
        changed += queued;
        if decider_broken {
            break;
        }
        tokio::task::yield_now().await;
    }
    if jobs > 0 {
        state.dispatcher.wake();
    }
    Ok(changed)
}

/// What [`recover_leftovers`] did.
#[derive(Debug, Default)]
pub struct Recovered {
    /// Originals put back from their backup.
    pub restored: Vec<PathBuf>,
    /// Originals whose backup was left alone: their job may still put its
    /// new file in place, or it isn't known yet whether it did. They are
    /// not gone.
    pub kept: Vec<PathBuf>,
}

impl Recovered {
    /// The originals that are not gone, though not under their name now or
    /// a moment ago.
    pub fn not_gone(self) -> impl Iterator<Item = PathBuf> {
        self.restored.into_iter().chain(self.kept)
    }
}

/// Hand leftover temp and backup files (a walk's `artifacts`) to the
/// worker's crash recovery: temp files are deleted, and an original moved
/// aside as a backup is put back when its name is free (the backup is
/// deleted otherwise). Files of jobs running right now, or whose new file
/// is still being put in place, are theirs and left alone, except those of
/// `own_job` (a job looking for what its interrupted run left, before it
/// starts working). So are those of a job whose new file may have been put
/// in place after it ended (marked in the database): the disk first tells
/// whether it got there (see `dispatcher::settle`), which, for a job that
/// can't run again by itself, is found out now; a job in the queue finds
/// out when it runs.
pub async fn recover_leftovers(
    state: &AppState,
    mut artifacts: Vec<PathBuf>,
    own_job: Option<Uuid>,
) -> Recovered {
    use crate::services::dispatcher::{SettleBy, Settled, settle};

    artifacts.sort();
    artifacts.dedup();
    let mut busy = state.dispatcher.running_ids();
    busy.extend(state.dispatcher.placing_ids());
    busy.retain(|id| Some(*id) != own_job);
    let marked: Vec<Uuid> = match db::jobs::placing(state.db.pool(), None).await {
        Ok(marked) => marked
            .into_iter()
            .map(|m| m.job.id)
            .filter(|id| Some(*id) != own_job)
            .collect(),
        Err(e) => {
            // Without knowing which backups may still be needed, none is
            // touched; the next search tries again.
            tracing::warn!("could not look for interrupted conversions: {e}");
            return Recovered::default();
        }
    };
    // Per marked job met: whether its files are settled (and so handled).
    let mut settled: HashMap<Uuid, bool> = HashMap::new();
    let mut out = Recovered::default();
    for path in artifacts {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let owned_by = |id: &Uuid| chrysopoeia_core::paths::is_artifact_of(&name, *id);
        let original = chrysopoeia_core::paths::is_backup(&name)
            .then(|| chrysopoeia_core::paths::original_name_from_backup(&name))
            .flatten()
            .map(|original| path.with_file_name(original));
        let left = if busy.iter().any(owned_by) {
            true
        } else if let Some(id) = marked.iter().copied().find(owned_by) {
            // Whatever the settle finds, the original isn't gone: it is
            // back, or its new file took its place (and the record says so).
            out.kept.extend(original.clone());
            if let std::collections::hash_map::Entry::Vacant(entry) = settled.entry(id) {
                let done = match settle(state, id, SettleBy::Other, PATH_CHECK_TIMEOUT).await {
                    Settled::Placed(_) => true,
                    Settled::NotPlaced(restored) => {
                        out.restored.extend(restored);
                        true
                    }
                    Settled::Unreachable { .. } | Settled::Left => false,
                };
                entry.insert(done);
            }
            // Settled, its files in its folders were handled with it; the
            // rest are found by the next search. Not settled: left alone.
            if settled.get(&id) == Some(&true) {
                continue;
            }
            true
        } else {
            false
        };
        if left {
            out.kept.extend(original);
            continue;
        }
        match state.toolkit.recover_artifact(path.clone()).await {
            Ok(Recovery::RestoredBackup(original)) => {
                state
                    .activity(
                        ActivityLevel::Warning,
                        format!(
                            "Restored {} from its backup after an interrupted conversion.",
                            original.display()
                        ),
                        ActivityRefs::default(),
                    )
                    .await;
                out.restored.push(original);
            }
            Ok(r) => tracing::debug!(path = %path.display(), "leftover: {r:?}"),
            Err(e) => {
                tracing::warn!(path = %path.display(), "could not clean up a leftover file: {e:#}")
            }
        }
    }
    out
}

/// Remove what a finished job left anywhere in its library: the temp file
/// it wrote next to its file, when that file's folder was renamed while it
/// was being converted (it ends up in the folder's new place, where the job
/// can't find it). Only that job's files are touched.
pub async fn sweep_job_leftovers(state: &AppState, library_id: Uuid, job_id: Uuid) {
    let Ok(Some(lib)) = db::libraries::get(state.db.pool(), library_id).await else {
        return;
    };
    let walked = state
        .toolkit
        .walk_library(PathBuf::from(&lib.path), scan_options(state))
        .await;
    let Ok(Ok(walk)) = walked else {
        return;
    };
    let mine: Vec<PathBuf> = walk
        .artifacts
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| chrysopoeia_core::paths::is_artifact_of(n, job_id))
        })
        .collect();
    if !mine.is_empty() {
        tracing::debug!(job = %job_id, "removing {} leftover file(s) of a moved file", mine.len());
        recover_leftovers(state, mine, None).await;
    }
}

/// The reason given for a folder that doesn't answer in time.
pub fn not_responding(path: &str) -> String {
    format!(
        "The folder {path} isn't responding. If it's on a network share or an external drive, \
         check the connection."
    )
}

/// What a look at a folder found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Folder {
    /// It can be used.
    Fine,
    /// It can't, and why: missing, not allowed, not a folder, empty, or not
    /// responding.
    Problem(String),
    /// It couldn't be looked at right now: checks of other shares that
    /// stopped answering hold every thread set aside for checks (see
    /// [`fs_guard::NoAnswer::Busy`]). Nothing is known about it; it is
    /// never taken for a folder with a problem.
    Unknown,
}

impl Folder {
    /// The problem, if one was found.
    pub fn problem(self) -> Option<String> {
        match self {
            Self::Problem(p) => Some(p),
            Self::Fine | Self::Unknown => None,
        }
    }

    fn from_check(path: &str, looked: Result<Option<String>, fs_guard::NoAnswer>) -> Self {
        match looked {
            Ok(None) => Self::Fine,
            Ok(Some(problem)) => Self::Problem(problem),
            Err(fs_guard::NoAnswer::NotAnswering) => Self::Problem(not_responding(path)),
            Err(fs_guard::NoAnswer::Busy) => Self::Unknown,
        }
    }
}

/// Whether a library's folder can be used right now. A folder that doesn't
/// answer within a few seconds is reported as not responding; the check
/// itself runs on one thread per folder at a time, and a share that stopped
/// answering holds only a few (see [`fs_guard`]), so it can't use up the
/// server's threads or the checks of other folders.
pub async fn path_problem(path: &str) -> Folder {
    let p = path.to_string();
    let looked = fs_guard::guarded(
        "path_problem",
        Path::new(path),
        PATH_CHECK_TIMEOUT,
        move || check_path(&p),
    )
    .await;
    Folder::from_check(path, looked)
}

/// Whether a library folder can hold the files the queue expects right now:
/// [`path_problem`], and a folder with nothing in it at all, which for a
/// library with files almost always means an unmounted drive or share (the
/// mount point is left behind empty), is a problem too.
pub async fn root_unavailable(path: &str) -> Folder {
    let p = path.to_string();
    let looked = fs_guard::guarded(
        "root_unavailable",
        Path::new(path),
        PATH_CHECK_TIMEOUT,
        move || {
            check_path(&p).or_else(|| {
                let empty = std::fs::read_dir(&p).is_ok_and(|mut rd| rd.next().is_none());
                empty.then(|| {
                    format!(
                        "The folder {p} is empty. If it's on a drive or network share, check \
                         that it's connected."
                    )
                })
            })
        },
    )
    .await;
    Folder::from_check(path, looked)
}

/// [`path_problem`]'s check, with blocking calls.
fn check_path(path: &str) -> Option<String> {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(format!(
            "The folder {path} is missing. If it's on a drive or network share, check that it's connected; \
             in Docker, check that it's mapped into the container."
        )),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Some(format!(
            "Chrysopoeia doesn't have permission to open {path}. Check the folder's permissions \
             (and PUID/PGID in Docker)."
        )),
        Err(e) => Some(format!(
            "Chrysopoeia can't open the folder {path} because {}. If it's on a drive or network \
             share, check that it's connected.",
            chrysopoeia_core::plain::io_reason(&e)
        )),
        Ok(m) if !m.is_dir() => Some(format!("{path} is not a folder.")),
        Ok(_) => match std::fs::read_dir(path) {
            Ok(_) => None,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Some(format!(
                "Chrysopoeia doesn't have permission to read {path}. Check the folder's \
                 permissions (and PUID/PGID in Docker)."
            )),
            Err(e) => Some(format!(
                "Chrysopoeia can't read the folder {path} because {}. If it's on a drive or \
                 network share, check that it's connected.",
                chrysopoeia_core::plain::io_reason(&e)
            )),
        },
    }
}

fn assemble(
    row: LibraryRow,
    stats: LibraryStats,
    scanning: bool,
    path_error: Option<String>,
) -> Library {
    Library {
        id: row.id,
        name: row.name,
        path: row.path,
        enabled: row.enabled,
        profile: row.profile,
        stats,
        scanning,
        last_scan_at: row.last_scan_at,
        path_error,
        created_at: row.created_at,
    }
}

/// Why a library's folder can't be used: a problem with the folder itself,
/// or the reason the queue is waiting for it (e.g. a share mounted empty).
async fn library_problem(state: &AppState, row: &LibraryRow) -> Option<String> {
    match path_problem(&row.path).await {
        Folder::Problem(p) => Some(p),
        // Not looked at (too many checks stuck elsewhere): no problem is
        // claimed for it.
        Folder::Fine | Folder::Unknown => state.dispatcher.offline_reason(row.id),
    }
}

/// Every library with live stats.
pub async fn view_all(state: &AppState) -> sqlx::Result<Vec<Library>> {
    let rows = db::libraries::list(state.db.pool()).await?;
    let mut stats = db::stats::by_library(state.db.pool()).await?;
    let scanning = state.library.scanning_ids();
    // Check every folder at once, so one slow share doesn't add up.
    let problems = futures::future::join_all(rows.iter().map(|r| library_problem(state, r))).await;
    let mut out = Vec::with_capacity(rows.len());
    for (row, problem) in rows.into_iter().zip(problems) {
        let s = stats.remove(&row.id).unwrap_or_default();
        let is_scanning = scanning.contains(&row.id);
        out.push(assemble(row, s, is_scanning, problem));
    }
    Ok(out)
}

/// One library with live stats.
pub async fn view_by_id(state: &AppState, id: Uuid) -> sqlx::Result<Option<Library>> {
    let Some(row) = db::libraries::get(state.db.pool(), id).await? else {
        return Ok(None);
    };
    let stats = db::stats::for_library(state.db.pool(), id).await?;
    let problem = library_problem(state, &row).await;
    let scanning = state.library.is_scanning(id);
    Ok(Some(assemble(row, stats, scanning, problem)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_prefix_matching_is_per_component() {
        let dirs = vec![PathBuf::from("/m/Locked")];
        assert!(under_unreadable("/m/Locked/a.mkv", &dirs));
        assert!(!under_unreadable("/m/LockedOut/a.mkv", &dirs));
    }

    #[test]
    fn notes_on_the_library_folder_itself_name_no_path() {
        let root = Path::new("/m/Movies");
        let pattern = (
            root.to_path_buf(),
            "The ignore pattern \"!x\" is not used.".to_string(),
        );
        let disc = (
            root.join("Film/VIDEO_TS"),
            "This folder is a DVD copy".to_string(),
        );
        assert_eq!(
            describe_problems(root, &[&pattern, &disc]),
            "The ignore pattern \"!x\" is not used; Film/VIDEO_TS: This folder is a DVD copy."
        );
    }

    #[test]
    fn scan_failures_read_as_sentences_with_advice_once() {
        let scanner = anyhow::anyhow!(
            "The folder /media/Movies does not exist. Check the path, and when running in \
             Docker that the folder is mounted into the container."
        );
        let text = scan_failure(&scanner);
        assert!(!text.contains(".."), "{text}");
        assert_eq!(text.matches("Check").count(), 1, "{text}");
        let other = anyhow::anyhow!("not a folder");
        assert_eq!(
            scan_failure(&other),
            "Not a folder. Check that the folder exists and its drive or share is connected."
        );
    }

    #[test]
    fn ignore_patterns_match_relative_paths() {
        let set = compile_ignore(&["**/Extras/**".to_string(), "[".to_string()]);
        assert!(set.is_match("Movie/Extras/clip.mkv"));
        assert!(!set.is_match("Movie/movie.mkv"));
    }
}

/// Drives and shares mounted inside a library folder.
///
/// When one is unmounted (a USB drive pulled, a share gone), its mount
/// point is left behind as an empty folder, and a scan would take every
/// file on it for deleted: their rows, and decisions such as "Skipped by
/// you", would be lost, and the files converted as new ones when the drive
/// is back. So scans remember the mount points inside each library (from
/// `/proc/self/mountinfo`), and one that is no longer mounted and is empty
/// or missing counts as offline: its files stay listed. It is forgotten
/// once the folder has files of its own without being a mount.
pub mod mounts {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    use crate::db;
    use crate::db::libraries::LibraryRow;
    use crate::services::fs_guard;
    use crate::state::AppState;

    /// How the scan describes an offline mount.
    pub const OFFLINE_REASON: &str = "the drive or share mounted there isn't connected, so its \
        files stay in the list until it's back";

    /// Mount points inside a library that are offline now (see the module
    /// docs), after updating the library's known mounts.
    pub async fn check(state: &AppState, lib: &LibraryRow) -> Vec<PathBuf> {
        let known = match db::libraries::mounts(state.db.pool(), lib.id).await {
            Ok(known) => known,
            Err(e) => {
                tracing::warn!(library = %lib.name, "could not read the known mounts: {e}");
                Vec::new()
            }
        };
        let root = PathBuf::from(&lib.path);
        let known: Vec<PathBuf> = known
            .into_iter()
            .map(PathBuf::from)
            .filter(|p| p.starts_with(&root) && *p != root)
            .collect();
        let (look_root, look_known) = (root.clone(), known.clone());
        let looked = fs_guard::guarded("mounts", &root, super::PATH_CHECK_TIMEOUT, move || {
            let current = mount_points_under(&look_root);
            classify(&look_known, &current, empty_or_missing)
        })
        .await;
        // A folder that doesn't answer: keep every known mount's files.
        let (keep, offline) = looked.unwrap_or_else(|_| (known.clone(), known.clone()));
        let keep: Vec<String> = keep
            .iter()
            .filter_map(|p| p.to_str().map(str::to_string))
            .collect();
        let changed = {
            let before: BTreeSet<&Path> = known.iter().map(PathBuf::as_path).collect();
            let after: BTreeSet<&Path> = keep.iter().map(Path::new).collect();
            before != after
        };
        if changed {
            let saved = async {
                let mut tx = state.db.write_tx().await?;
                db::libraries::set_mounts(&mut tx, lib.id, &keep).await?;
                tx.commit().await
            }
            .await;
            if let Err(e) = saved {
                tracing::warn!(library = %lib.name, "could not record the mounts: {e}");
            }
        }
        offline
    }

    /// The mounts to remember (mounted now, or offline) and the offline
    /// ones: known mounts not mounted now whose folder is empty or missing.
    pub fn classify(
        known: &[PathBuf],
        current: &[PathBuf],
        empty_or_missing: impl Fn(&Path) -> bool,
    ) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let offline: Vec<PathBuf> = known
            .iter()
            .filter(|k| !current.contains(k) && empty_or_missing(k))
            .cloned()
            .collect();
        let mut keep: Vec<PathBuf> = current.iter().chain(&offline).cloned().collect();
        keep.sort();
        keep.dedup();
        (keep, offline)
    }

    fn empty_or_missing(path: &Path) -> bool {
        match std::fs::read_dir(path) {
            Ok(mut entries) => entries.next().is_none(),
            Err(e) => e.kind() == std::io::ErrorKind::NotFound,
        }
    }

    /// Mount points strictly inside `root`.
    pub fn mount_points_under(root: &Path) -> Vec<PathBuf> {
        #[cfg_attr(not(test), allow(unused_mut))]
        let mut found: Vec<PathBuf> = std::fs::read_to_string("/proc/self/mountinfo")
            .map(|text| parse_mountinfo(&text))
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.starts_with(root) && p != root)
            .collect();
        #[cfg(test)]
        found.extend(fake::mounted_under(root));
        found.sort();
        found.dedup();
        found
    }

    /// The mount points listed in `/proc/self/mountinfo` (the fifth field,
    /// with `\040`-style escapes for spaces and the like).
    pub fn parse_mountinfo(text: &str) -> Vec<PathBuf> {
        text.lines()
            .filter_map(|line| line.split(' ').nth(4))
            .map(|field| PathBuf::from(unescape(field)))
            .collect()
    }

    fn unescape(field: &str) -> String {
        let bytes = field.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\\'
                && let Some(octal) = bytes.get(i + 1..i + 4)
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
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Tests stand in for mounting a drive inside a library.
    #[cfg(test)]
    pub mod fake {
        use std::collections::HashSet;
        use std::path::{Path, PathBuf};
        use std::sync::{LazyLock, Mutex};

        use crate::state::lock;

        static MOUNTED: LazyLock<Mutex<HashSet<PathBuf>>> = LazyLock::new(Mutex::default);

        /// Count `path` as a mount point.
        pub fn mount(path: &Path) {
            lock(&MOUNTED).insert(path.to_path_buf());
        }

        /// No longer.
        pub fn unmount(path: &Path) {
            lock(&MOUNTED).remove(path);
        }

        pub(super) fn mounted_under(root: &Path) -> Vec<PathBuf> {
            lock(&MOUNTED)
                .iter()
                .filter(|p| p.starts_with(root) && *p != root)
                .cloned()
                .collect()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn mountinfo_lists_mount_points() {
            let text = "22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
                36 22 0:32 / /media/Movies/USB\\040Drive rw - vfat /dev/sdb1 rw\n\
                37 22 0:33 / /media/Movies/Share rw - nfs server:/x rw\n";
            assert_eq!(
                parse_mountinfo(text),
                [
                    PathBuf::from("/"),
                    PathBuf::from("/media/Movies/USB Drive"),
                    PathBuf::from("/media/Movies/Share"),
                ]
            );
            assert_eq!(unescape("a\\134b\\011c\\x"), "a\\b\tc\\x");
        }

        #[test]
        fn unmounted_empty_mount_points_are_offline() {
            let usb = PathBuf::from("/m/USB");
            let nas = PathBuf::from("/m/NAS");
            let old = PathBuf::from("/m/Old");
            let empty = |p: &Path| p != Path::new("/m/Old");
            // USB unmounted and empty: offline. NAS mounted. Old unmounted
            // but has files of its own now: forgotten.
            let (keep, offline) = classify(
                &[usb.clone(), nas.clone(), old.clone()],
                &[nas.clone(), PathBuf::from("/m/New")],
                empty,
            );
            assert_eq!(offline, std::slice::from_ref(&usb));
            assert_eq!(
                keep,
                [PathBuf::from("/m/NAS"), PathBuf::from("/m/New"), usb]
            );
        }
    }
}
