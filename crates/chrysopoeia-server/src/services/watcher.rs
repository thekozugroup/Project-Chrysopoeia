//! Folder watching: keeps the watcher in line with the `watch_folders`
//! setting and the enabled libraries, and feeds settled events to the
//! library service.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use chrysopoeia_core::ActivityLevel;
use tokio::task::JoinHandle;

use crate::db;
use crate::db::activity::ActivityRefs;
use crate::services::library;
use crate::state::{AppState, lock};
use crate::toolkit::FolderWatcher;

/// How long a file's size must stay unchanged before it is picked up, so
/// files still being copied are not probed half-written.
pub const SETTLE: Duration = Duration::from_secs(20);

/// A running watcher and the task consuming its events.
pub struct ActiveWatcher {
    watcher: Box<dyn FolderWatcher>,
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

/// Start, stop or re-target the watcher to match the settings and the
/// enabled libraries. Problems are reported in the activity feed.
pub async fn sync(state: &AppState) {
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
    {
        let mut guard = lock(&state.library.watcher);
        if guard.is_none() {
            match state.toolkit.start_watcher(SETTLE) {
                Ok((watcher, mut rx)) => {
                    let consumer = state.clone();
                    let task = tokio::spawn(async move {
                        while let Some(event) = rx.recv().await {
                            if let Err(e) = library::handle_watch_event(&consumer, event).await {
                                tracing::error!("could not apply a folder change: {e:#}");
                            }
                        }
                    });
                    *guard = Some(ActiveWatcher {
                        watcher,
                        task,
                        roots: HashSet::new(),
                    });
                }
                Err(e) => problems.push(format!(
                    "Folder watching isn't available ({e:#}). New files will still be found by \
                     the regular rescans."
                )),
            }
        }
        if let Some(active) = guard.as_mut() {
            let wanted_roots: HashSet<PathBuf> = wanted.iter().map(|(p, _)| p.clone()).collect();
            let stale: Vec<PathBuf> = active.roots.difference(&wanted_roots).cloned().collect();
            for root in stale {
                let watcher = &active.watcher;
                if let Err(e) = state.toolkit.watcher_call(|| watcher.unwatch(&root)) {
                    tracing::debug!(root = %root.display(), "unwatch failed: {e:#}");
                }
                active.roots.remove(&root);
            }
            for (root, name) in &wanted {
                if active.roots.contains(root) {
                    continue;
                }
                let watcher = &active.watcher;
                match state.toolkit.watcher_call(|| watcher.watch(root)) {
                    Ok(()) => {
                        active.roots.insert(root.clone());
                    }
                    Err(e) => problems.push(format!(
                        "Chrysopoeia can't watch {name} for new files ({e:#}). New files will \
                         still be found by the regular rescans. On Linux, raising \
                         fs.inotify.max_user_watches on the host usually fixes this."
                    )),
                }
            }
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
