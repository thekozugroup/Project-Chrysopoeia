//! Folder watching: keeps the watcher in line with the `watch_folders`
//! setting, the enabled libraries and the scan filters (ignore patterns and
//! minimum size), and feeds settled events to the library service.
//!
//! Adding a watch walks the whole folder tree (one inotify watch per folder),
//! which takes seconds on big or slow shares, so every watcher call runs on a
//! blocking thread and no lock is held across it: the watcher's own methods
//! are safe to call from several threads.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrysopoeia_core::ActivityLevel;
use chrysopoeia_scanner::ScanOptions;
use tokio::task::JoinHandle;

use crate::db;
use crate::db::activity::ActivityRefs;
use crate::services::library;
use crate::state::{AppState, lock};
use crate::toolkit::FolderWatcher;

/// How often the watcher's count of files still being copied is looked at.
const WAITING_POLL: Duration = Duration::from_secs(2);

/// The filters a root is watched with, to notice when settings change them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Filters {
    ignore_patterns: Vec<String>,
    min_size_bytes: u64,
}

impl Filters {
    fn of(opts: &ScanOptions) -> Self {
        Self {
            ignore_patterns: opts.ignore_patterns.clone(),
            min_size_bytes: opts.min_size_bytes,
        }
    }
}

/// A running watcher and the task consuming its events.
pub struct ActiveWatcher {
    watcher: Arc<dyn FolderWatcher>,
    task: JoinHandle<()>,
    /// Watched roots, with the filters they were watched with.
    roots: HashMap<PathBuf, Filters>,
}

impl Drop for ActiveWatcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The watcher's own explanation (a sentence with its fix), without error
/// numbers, and saying that rescans still find new files.
fn with_rescan_note(reason: &str) -> String {
    let reason = chrysopoeia_core::plain::strip_os_error(reason);
    let reason = reason.trim();
    let mut out = reason.to_string();
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    if !reason.contains("rescans") {
        out.push_str(" New files will still be found by the regular rescans.");
    }
    out
}

/// Start the watcher (on a blocking thread) and the task that applies its
/// events. Returns a problem to report when watching isn't available.
async fn start(state: &AppState) -> Result<ActiveWatcher, String> {
    let toolkit = state.toolkit.clone();
    let settle = state.config.settle;
    let started = tokio::task::spawn_blocking(move || toolkit.start_watcher(settle)).await;
    let (watcher, mut rx) = match started {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            return Err(format!(
                "Folder watching isn't available. {}",
                with_rescan_note(&format!("{e:#}"))
            ));
        }
        Err(e) => {
            tracing::warn!("the folder watcher could not start: {e}");
            return Err(
                "Folder watching isn't available because it stopped unexpectedly. New \
                        files will still be found by the regular rescans."
                    .to_string(),
            );
        }
    };
    let consumer = state.clone();
    let waiting = watcher.waiting_files();
    let task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(WAITING_POLL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last: HashMap<PathBuf, usize> = HashMap::new();
        loop {
            tokio::select! {
                event = rx.recv() => {
                    let Some(event) = event else { break };
                    if let Err(e) = library::handle_watch_event(&consumer, event).await {
                        tracing::error!("could not apply a folder change: {e:#}");
                    }
                }
                _ = tick.tick(), if waiting.is_some() => {
                    // Copies in progress show as files still being copied;
                    // the files Chrysopoeia puts in place itself are not.
                    let files = waiting.as_ref().map(|w| w.files()).unwrap_or_default();
                    let now = match library::settling_copies(&consumer, &files).await {
                        Ok(now) => now,
                        Err(e) => {
                            tracing::debug!(
                                "could not tell which files are still being copied: {e}"
                            );
                            continue;
                        }
                    };
                    if now != last {
                        match library::set_watch_settling(&consumer, &now).await {
                            Ok(()) => last = now,
                            Err(e) => tracing::debug!(
                                "could not record the files still being copied: {e}"
                            ),
                        }
                    }
                }
            }
        }
    });
    Ok(ActiveWatcher {
        watcher: Arc::from(watcher),
        task,
        roots: HashMap::new(),
    })
}

/// What [`sync`] changes, decided under the lock and done without it.
struct Plan {
    watcher: Arc<dyn FolderWatcher>,
    stale: Vec<PathBuf>,
    /// Roots to watch, or to watch again with new filters.
    watch: Vec<(PathBuf, String)>,
}

/// Start, stop or re-target the watcher to match the settings and the
/// enabled libraries. Problems are reported in the activity feed.
pub async fn sync(state: &AppState) {
    // One sync at a time; the watcher calls themselves run unlocked.
    let _serial = state.library.watcher_sync.lock().await;
    let settings = state.settings();
    if !settings.watch_folders || state.shutdown.is_cancelled() {
        stop(state);
        return;
    }
    let libs = match db::libraries::list(state.db.pool()).await {
        Ok(libs) => libs,
        Err(e) => {
            tracing::error!("could not list libraries for the watcher: {e}");
            return;
        }
    };
    let wanted: Vec<(PathBuf, String)> = libs
        .into_iter()
        .filter(|l| l.enabled)
        .map(|l| (PathBuf::from(l.path), l.name))
        .collect();
    let opts = ScanOptions::from_settings(&settings);
    let filters = Filters::of(&opts);
    let mut problems: Vec<String> = Vec::new();

    if lock(&state.library.watcher).is_none() {
        match start(state).await {
            Ok(active) => *lock(&state.library.watcher) = Some(active),
            Err(problem) => problems.push(problem),
        }
    }

    let plan = lock(&state.library.watcher).as_ref().map(|active| {
        let stale: Vec<PathBuf> = active
            .roots
            .keys()
            .filter(|r| !wanted.iter().any(|(p, _)| p == *r))
            .cloned()
            .collect();
        let watch: Vec<(PathBuf, String)> = wanted
            .iter()
            .filter(|(p, _)| active.roots.get(p) != Some(&filters))
            .cloned()
            .collect();
        Plan {
            watcher: Arc::clone(&active.watcher),
            stale,
            watch,
        }
    });
    if let Some(plan) = plan
        && (!plan.stale.is_empty() || !plan.watch.is_empty())
    {
        let toolkit = state.toolkit.clone();
        let applied = tokio::task::spawn_blocking(move || {
            let watcher = &plan.watcher;
            for root in &plan.stale {
                if let Err(e) = toolkit.watcher_call(|| watcher.unwatch(root)) {
                    tracing::debug!(root = %root.display(), "unwatch failed: {e:#}");
                }
            }
            let added: Vec<(PathBuf, String, Result<(), String>)> = plan
                .watch
                .into_iter()
                .map(|(root, name)| {
                    let result = toolkit
                        .watcher_call(|| watcher.watch_with_options(&root, &opts))
                        .map_err(|e| format!("{e:#}"));
                    (root, name, result)
                })
                .collect();
            (plan.watcher, plan.stale, added)
        })
        .await;
        match applied {
            Ok((handle, stale, added)) => {
                let mut guard = lock(&state.library.watcher);
                // Only record roots on the watcher they were added to (it may
                // have been stopped meanwhile).
                let active = guard.as_mut().filter(|a| Arc::ptr_eq(&a.watcher, &handle));
                if let Some(active) = active {
                    for root in &stale {
                        active.roots.remove(root);
                    }
                    for (root, _, result) in &added {
                        if result.is_ok() {
                            active.roots.insert(root.clone(), filters.clone());
                        }
                    }
                }
                drop(guard);
                for (_, name, result) in added {
                    if let Err(e) = result {
                        problems.push(format!(
                            "Chrysopoeia can't watch {name} for new files. {}",
                            with_rescan_note(&e)
                        ));
                    }
                }
            }
            Err(e) => tracing::error!("the folder watcher stopped unexpectedly: {e}"),
        }
    }

    let new_problems: Vec<String> = {
        let mut reported = lock(&state.library.reported_watch_problems);
        problems
            .into_iter()
            .filter(|m| reported.insert(m.clone()))
            .collect()
    };
    for message in new_problems {
        state
            .activity(ActivityLevel::Warning, message, ActivityRefs::default())
            .await;
    }
}

/// Stop watching entirely. The watcher itself is dropped on a blocking
/// thread: dropping it closes one kernel watcher per library.
pub fn stop(state: &AppState) {
    let old = lock(&state.library.watcher).take();
    lock(&state.library.reported_watch_problems).clear();
    if let Some(old) = old {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(move || drop(old));
                // Nothing is waited on for the watcher any more.
                let state = state.clone();
                handle.spawn(async move {
                    if let Err(e) = library::set_watch_settling(&state, &HashMap::new()).await {
                        tracing::debug!("could not clear the files still being copied: {e}");
                    }
                });
            }
            Err(_) => drop(old),
        }
    }
}
