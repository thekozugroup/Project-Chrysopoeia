//! Folder watching: keeps the watcher in line with the `watch_folders`
//! setting and the enabled libraries, and feeds settled events to the
//! library service.
//!
//! Adding a watch walks the whole folder tree (one inotify watch per folder),
//! which takes seconds on big or slow shares, so every watcher call runs on a
//! blocking thread and no lock is held across it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrysopoeia_core::ActivityLevel;
use tokio::task::JoinHandle;

use crate::db;
use crate::db::activity::ActivityRefs;
use crate::services::library;
use crate::state::{AppState, lock};
use crate::toolkit::FolderWatcher;

/// A running watcher and the task consuming its events.
pub struct ActiveWatcher {
    watcher: Arc<Mutex<Box<dyn FolderWatcher>>>,
    task: JoinHandle<()>,
    roots: HashSet<PathBuf>,
}

impl Drop for ActiveWatcher {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Paths currently watched (for tests and diagnostics).
pub fn watched_roots(state: &AppState) -> Vec<PathBuf> {
    let guard = lock(&state.library.watcher);
    let mut roots: Vec<PathBuf> = guard
        .as_ref()
        .map(|w| w.roots.iter().cloned().collect())
        .unwrap_or_default();
    roots.sort();
    roots
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
                "Folder watching isn't available ({e:#}). New files will still be found by the \
                 regular rescans."
            ));
        }
        Err(e) => {
            return Err(format!(
                "Folder watching isn't available ({e}). New files will still be found by the \
                 regular rescans."
            ));
        }
    };
    let consumer = state.clone();
    let task = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if let Err(e) = library::handle_watch_event(&consumer, event).await {
                tracing::error!("could not apply a folder change: {e:#}");
            }
        }
    });
    Ok(ActiveWatcher {
        watcher: Arc::new(Mutex::new(watcher)),
        task,
        roots: HashSet::new(),
    })
}

/// Start, stop or re-target the watcher to match the settings and the
/// enabled libraries. Problems are reported in the activity feed.
pub async fn sync(state: &AppState) {
    // One sync at a time; the watcher calls themselves run unlocked.
    let _serial = state.library.watcher_sync.lock().await;
    if !state.settings().watch_folders || state.shutdown.is_cancelled() {
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
    let mut problems: Vec<String> = Vec::new();

    if lock(&state.library.watcher).is_none() {
        match start(state).await {
            Ok(active) => *lock(&state.library.watcher) = Some(active),
            Err(problem) => problems.push(problem),
        }
    }

    // What to change, decided under the lock; done without it.
    let plan = lock(&state.library.watcher).as_ref().map(|active| {
        let wanted_roots: HashSet<&PathBuf> = wanted.iter().map(|(p, _)| p).collect();
        let stale: Vec<PathBuf> = active
            .roots
            .iter()
            .filter(|r| !wanted_roots.contains(r))
            .cloned()
            .collect();
        let missing: Vec<(PathBuf, String)> = wanted
            .iter()
            .filter(|(p, _)| !active.roots.contains(p))
            .cloned()
            .collect();
        (Arc::clone(&active.watcher), stale, missing)
    });
    if let Some((handle, stale, missing)) = plan
        && (!stale.is_empty() || !missing.is_empty())
    {
        let toolkit = state.toolkit.clone();
        let applied = tokio::task::spawn_blocking(move || {
            let watcher = lock(&handle);
            for root in &stale {
                if let Err(e) = toolkit.watcher_call(|| watcher.unwatch(root)) {
                    tracing::debug!(root = %root.display(), "unwatch failed: {e:#}");
                }
            }
            let added: Vec<(PathBuf, String, Result<(), String>)> = missing
                .into_iter()
                .map(|(root, name)| {
                    let result = toolkit
                        .watcher_call(|| watcher.watch(&root))
                        .map_err(|e| format!("{e:#}"));
                    (root, name, result)
                })
                .collect();
            drop(watcher);
            (handle, stale, added)
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
                            active.roots.insert(root.clone());
                        }
                    }
                }
                drop(guard);
                for (_, name, result) in added {
                    if let Err(e) = result {
                        problems.push(format!(
                            "Chrysopoeia can't watch {name} for new files ({e}). New files will \
                             still be found by the regular rescans. On Linux, raising \
                             fs.inotify.max_user_watches on the host usually fixes this."
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

/// Stop watching entirely.
pub fn stop(state: &AppState) {
    let old = lock(&state.library.watcher).take();
    drop(old);
    lock(&state.library.reported_watch_problems).clear();
}
