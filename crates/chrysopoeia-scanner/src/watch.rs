//! Filesystem watcher using the `notify` crate.
//!
//! Watches library directories for new, modified, or deleted media files
//! and emits events via a tokio channel.

use std::path::PathBuf;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

/// Events emitted by the file watcher.
#[derive(Debug, Clone)]
pub enum FileEvent {
    /// A new file was created.
    Created(PathBuf),
    /// An existing file was modified.
    Modified(PathBuf),
    /// A file was removed.
    Removed(PathBuf),
}

/// Watches library directories for filesystem changes.
pub struct FileWatcher {
    _watcher: RecommendedWatcher,
    /// Receive file events from this channel.
    pub receiver: mpsc::Receiver<FileEvent>,
}

impl FileWatcher {
    /// Create a new file watcher for the given directories.
    ///
    /// Returns the watcher and a channel receiver for file events.
    pub fn new(directories: &[PathBuf]) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::channel(256);

        let sender = tx.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    let file_events = translate_event(event);
                    for fe in file_events {
                        let _ = sender.blocking_send(fe);
                    }
                }
            })?;

        for dir in directories {
            tracing::info!("Watching directory: {}", dir.display());
            watcher.watch(dir, RecursiveMode::Recursive)?;
        }

        Ok(Self {
            _watcher: watcher,
            receiver: rx,
        })
    }
}

/// Translate a notify event into our domain events.
fn translate_event(event: notify::Event) -> Vec<FileEvent> {
    use notify::EventKind;

    let mut out = Vec::new();
    for path in event.paths {
        match event.kind {
            EventKind::Create(_) => out.push(FileEvent::Created(path)),
            EventKind::Modify(_) => out.push(FileEvent::Modified(path)),
            EventKind::Remove(_) => out.push(FileEvent::Removed(path)),
            _ => {}
        }
    }
    out
}
