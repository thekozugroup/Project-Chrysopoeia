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
    ActivityLevel, Event, FileStatus, Library, LibraryStats, ProbeInfo, ScanPhase, ScanProgress,
    TranscodeProfile,
};
use chrysopoeia_scanner::{DiscoveredFile, ProbeError, ScanOptions, WatchEvent};
use chrysopoeia_worker::Decision;
use futures::StreamExt;
use globset::{Glob, GlobSet, GlobSetBuilder};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::activity::ActivityRefs;
use crate::db::files::{FileUpsert, IndexEntry};
use crate::db::jobs::NewJob;
use crate::db::libraries::LibraryRow;
use crate::db::{self, ts};
use crate::format::{count, plural};
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
    probes: Arc<Semaphore>,
}

impl Default for LibraryHandle {
    fn default() -> Self {
        Self {
            scans: std::sync::Mutex::new(HashMap::new()),
            watcher: std::sync::Mutex::new(None),
            watcher_sync: tokio::sync::Mutex::new(()),
            reported_watch_problems: std::sync::Mutex::new(HashSet::new()),
            probes: Arc::new(Semaphore::new(PROBE_CONCURRENCY)),
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

/// Options for walking a library under the current settings.
fn scan_options(state: &AppState) -> ScanOptions {
    let settings = state.settings();
    ScanOptions {
        ignore_patterns: settings.ignore_patterns,
        min_size_bytes: u64::from(settings.min_file_size_mb) * 1_000_000,
        follow_links: false,
    }
}

/// Plain-language message for a probe failure.
pub fn probe_error_message(e: &ProbeError) -> String {
    match e {
        ProbeError::Spawn(msg) => {
            format!("ffprobe couldn't be started, so this file wasn't checked: {msg}")
        }
        ProbeError::Timeout(_) => "Reading this file took longer than 60 seconds. It may be \
            damaged, or the drive may be very slow."
            .to_string(),
        ProbeError::Unreadable(msg) => {
            format!("This file can't be read as a video. It may be damaged or incomplete: {msg}")
        }
        ProbeError::Parse(msg) => {
            format!("ffprobe's answer about this file couldn't be understood: {msg}")
        }
    }
}

/// What a probed file needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Transcode,
    Skip(String),
    Broken(String),
}

/// Decide what a file needs, turning tool failures into a `Broken` verdict.
pub fn verdict(
    state: &AppState,
    probe: &Result<ProbeInfo, ProbeError>,
    profile: &TranscodeProfile,
) -> Verdict {
    match probe {
        Err(e) => Verdict::Broken(probe_error_message(e)),
        Ok(p) => match state.toolkit.decide(p, profile) {
            Ok(Decision::Transcode) => Verdict::Transcode,
            Ok(Decision::Skip { reason }) => Verdict::Skip(reason),
            Err(_) => Verdict::Broken(
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

/// The row to store for a probed and decided file.
fn build_upsert(
    library_id: Uuid,
    root: &Path,
    file: &DiscoveredFile,
    probe: Result<ProbeInfo, ProbeError>,
    verdict: Verdict,
    auto_queue: bool,
) -> Option<FileUpsert> {
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
    let (status, skip_reason, error) = match verdict {
        Verdict::Transcode if auto_queue => (FileStatus::Queued, None, None),
        Verdict::Transcode => (FileStatus::Pending, None, None),
        Verdict::Skip(reason) => (FileStatus::Skipped, Some(reason), None),
        Verdict::Broken(error) => (FileStatus::Failed, None, Some(error)),
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
/// (moved, renamed, or replaced by a conversion). Such files are not
/// reported as broken; the next scan sees them as they are.
async fn vanished(probe: &Result<ProbeInfo, ProbeError>, path: &Path) -> bool {
    probe.is_err()
        && matches!(
            tokio::fs::symlink_metadata(path).await,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound
        )
}

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
    /// The profile the batch was decided with.
    profile: TranscodeProfile,
    broken: u64,
    jobs: usize,
}

/// Decide a batch of probed files with the library's current profile and
/// store it.
async fn flush(
    state: &AppState,
    lib: &LibraryRow,
    batch: Vec<Probed>,
    read_at: &str,
) -> sqlx::Result<Flushed> {
    let profile = db::libraries::get(state.db.pool(), lib.id)
        .await?
        .map_or_else(|| lib.profile.clone(), |l| l.profile);
    let auto_queue = state.settings().auto_queue;
    let root = Path::new(&lib.path);
    let mut broken = 0;
    let mut analyzed = Vec::with_capacity(batch.len());
    for p in batch {
        let v = verdict(state, &p.probe, &profile);
        if matches!(v, Verdict::Broken(_)) {
            broken += 1;
        }
        if let Some(upsert) = build_upsert(lib.id, root, &p.file, p.probe, v, auto_queue) {
            analyzed.push(Analyzed {
                existing: p.existing,
                upsert,
            });
        }
    }
    let (_, jobs) = write_batch(state, analyzed, read_at).await?;
    Ok(Flushed {
        profile,
        broken,
        jobs,
    })
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

    let walk = match state
        .toolkit
        .walk_library(PathBuf::from(&lib.path), scan_options(state))
        .await
    {
        Ok(Ok(walk)) => walk,
        Ok(Err(e)) => {
            state
                .library_activity(
                    ActivityLevel::Error,
                    format!(
                        "Couldn't scan {}: {e:#}. Check that the folder exists and its drive \
                         or share is connected.",
                        lib.name
                    ),
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
    let discovered = walk.files.len() as u64;
    state.emit(scan_progress(
        &lib,
        ScanPhase::Discovering,
        discovered,
        0,
        0,
    ));
    for (path, reason) in walk.errors.iter().take(5) {
        tracing::warn!(library = %lib.name, path = %path.display(), "unreadable: {reason}");
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

    // Files that disappeared. An empty walk of a library that had files is
    // almost always an unmounted drive, so nothing is removed then.
    let unreadable: Vec<PathBuf> = walk.errors.iter().map(|(p, _)| p.clone()).collect();
    let removed: Vec<IndexEntry> = index
        .values()
        .filter(|e| !seen.contains(e.path.as_str()))
        .filter(|e| e.status != FileStatus::Processing)
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
            removed_count += db::files::delete_unchanged(&mut tx, chunk).await?;
            tx.commit().await?;
        }
    }

    // Probe new and changed files, then decide and store them in batches.
    // Each batch is decided with the profile read right before it is
    // written, so a goal changed during a long scan applies to the rest.
    let mut profile = lib.profile.clone();
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
            profile = flushed.profile;
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
        profile = flushed.profile;
    }
    if cancel.is_cancelled() {
        return Ok(());
    }

    // The goal changed after the last batch was decided (the change itself
    // re-decided only the files that were stored by then).
    let mut redecided = 0;
    if let Some(current) = db::libraries::get(state.db.pool(), id).await?
        && current.profile != profile
    {
        redecided = redecide(state, &current, Some(&profile)).await?;
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
    recheck_settling(state, settling);
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
            let libs = db::files::delete_path_or_prefix(state.db.pool(), p).await?;
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
    let root = PathBuf::from(&lib.path);
    if let Ok(rel) = path.strip_prefix(&root)
        && compile_ignore(&settings.ignore_patterns).is_match(rel)
    {
        return Ok(Single::Handled);
    }
    let Ok(meta) = tokio::fs::metadata(path).await else {
        return Ok(Single::Handled);
    };
    if !meta.is_file() || meta.len() < u64::from(settings.min_file_size_mb) * 1_000_000 {
        return Ok(Single::Handled);
    }
    let modified: DateTime<Utc> = meta
        .modified()
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|_| Utc::now());
    let read_at = db::now_ts();
    let existing = db::files::find_by_path(state.db.pool(), path_str).await?;
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
    let probe = probe_limited(state, file.path.clone()).await;
    if vanished(&probe, &file.path).await {
        return Ok(Single::Handled);
    }
    let v = verdict(state, &probe, &lib.profile);
    let Some(upsert) = build_upsert(lib.id, &root, &file, probe, v, settings.auto_queue) else {
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
fn affects_output_size(old: &TranscodeProfile, new: &TranscodeProfile) -> bool {
    old.video_codec != new.video_codec
        || old.audio_codec != new.audio_codec
        || old.quality != new.quality
        || old.quality_override != new.quality_override
        || old.speed != new.speed
        || old.max_height != new.max_height
        || old.min_savings_pct != new.min_savings_pct
        || old.audio_languages != new.audio_languages
}

/// Re-decide the `pending` and `skipped` files of a library after its profile
/// changed from `previous` (when known). Files a job skipped as not worth
/// keeping stay skipped unless the change affects output size. Works in
/// chunks of [`REDECIDE_CHUNK`] files, each read and written in one short
/// transaction. Returns how many files changed status.
pub async fn redecide(
    state: &AppState,
    lib: &LibraryRow,
    previous: Option<&TranscodeProfile>,
) -> anyhow::Result<u64> {
    let size_matters = previous.is_none_or(|p| affects_output_size(p, &lib.profile));
    let auto_queue = state.settings().auto_queue;
    let ids = db::files::redecide_candidate_ids(state.db.pool(), lib.id, USER_SKIP_REASON).await?;
    let mut changed = 0u64;
    let mut jobs = 0u64;
    let mut decider_broken = false;
    for chunk in ids.chunks(REDECIDE_CHUNK) {
        let mut tx = state.db.write_tx().await?;
        let candidates = db::files::redecide_candidates(&mut tx, chunk, USER_SKIP_REASON).await?;
        let mut to_queue = Vec::new();
        for c in candidates {
            if c.size_rule_skip && !size_matters {
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
                    &[FileStatus::Pending, FileStatus::Skipped],
                )
                .await?;
                if status != c.status {
                    changed += 1;
                }
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

/// Why a library's folder can't be used right now, if it can't.
pub async fn path_problem(path: &str) -> Option<String> {
    match tokio::time::timeout(PATH_CHECK_TIMEOUT, check_path(path)).await {
        Ok(problem) => problem,
        Err(_) => Some(format!(
            "The folder {path} isn't responding. If it's on a network share or an external \
             drive, check the connection."
        )),
    }
}

/// Why a library folder can't hold the files the queue expects right now:
/// [`path_problem`], or a folder with nothing in it at all, which for a
/// library with files almost always means an unmounted drive or share (the
/// mount point is left behind empty).
pub async fn root_unavailable(path: &str) -> Option<String> {
    if let Some(problem) = path_problem(path).await {
        return Some(problem);
    }
    let empty = async {
        match tokio::fs::read_dir(path).await {
            Ok(mut rd) => matches!(rd.next_entry().await, Ok(None)),
            Err(_) => false,
        }
    };
    match tokio::time::timeout(PATH_CHECK_TIMEOUT, empty).await {
        Ok(false) => None,
        Ok(true) => Some(format!(
            "The folder {path} is empty. If it's on a drive or network share, check that it's \
             connected."
        )),
        Err(_) => Some(format!(
            "The folder {path} isn't responding. If it's on a network share or an external \
             drive, check the connection."
        )),
    }
}

async fn check_path(path: &str) -> Option<String> {
    match tokio::fs::metadata(path).await {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(format!(
            "The folder {path} is missing. If it's on a drive or network share, check that it's connected."
        )),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Some(format!(
            "Chrysopoeia doesn't have permission to open {path}. Check the folder's permissions \
             (and PUID/PGID in Docker)."
        )),
        Err(e) => Some(format!("Chrysopoeia can't open {path}: {e}")),
        Ok(m) if !m.is_dir() => Some(format!("{path} is not a folder.")),
        Ok(_) => match tokio::fs::read_dir(path).await {
            Ok(_) => None,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Some(format!(
                "Chrysopoeia doesn't have permission to read {path}. Check the folder's \
                 permissions (and PUID/PGID in Docker)."
            )),
            Err(e) => Some(format!("Chrysopoeia can't read {path}: {e}")),
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
        Some(p) => Some(p),
        None => state.dispatcher.offline_reason(row.id),
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
    fn ignore_patterns_match_relative_paths() {
        let set = compile_ignore(&["**/Extras/**".to_string(), "[".to_string()]);
        assert!(set.is_match("Movie/Extras/clip.mkv"));
        assert!(!set.is_match("Movie/movie.mkv"));
    }
}
