//! Debounced folder watching. Implemented by the scanner agent.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::mpsc;

/// A settled change under a watched root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// A media file was created or modified and its size has stopped
    /// changing for the settle period (i.e. the copy finished).
    Upserted(PathBuf),
    /// A media file (or a folder containing media) was removed or moved away.
    Removed(PathBuf),
}

/// Watches library roots. Dropping it stops watching.
pub struct LibraryWatcher {
    _private: (),
}

impl LibraryWatcher {
    /// Start a watcher. `settle` is how long a file's size must stay
    /// unchanged before `Upserted` is emitted. Temp/backup artifacts and
    /// non-media files never produce events.
    pub fn start(settle: Duration) -> anyhow::Result<(Self, mpsc::Receiver<WatchEvent>)> {
        let _ = settle;
        todo!("implemented by the scanner agent")
    }

    /// Start watching a root recursively. Watching the same root twice is a no-op.
    pub fn watch(&self, root: &Path) -> anyhow::Result<()> {
        let _ = root;
        todo!("implemented by the scanner agent")
    }

    /// Stop watching a root. Unknown roots are a no-op.
    pub fn unwatch(&self, root: &Path) -> anyhow::Result<()> {
        let _ = root;
        todo!("implemented by the scanner agent")
    }
}
