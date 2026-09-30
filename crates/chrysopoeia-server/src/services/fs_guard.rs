//! Filesystem checks on folders that may hang.
//!
//! On a network share whose server went away (an NFS hard mount) or a stuck
//! FUSE mount, every system call on the folder blocks, sometimes for good.
//! `tokio::fs` runs each call on a blocking thread, and giving up on it with
//! a timeout leaves that thread stuck; repeated on every page load, stuck
//! threads pile up until the runtime's blocking pool (512 threads) is used
//! up and everything that touches a disk stops, healthy folders included.
//!
//! [`guarded`] runs one check per path (and kind of check) at a time: a
//! caller that comes while the same check is still running waits for its
//! answer, up to its own timeout, instead of starting another. At most
//! [`MAX_STUCK_CHECKS`] checks run at once; beyond that a check counts as
//! not answering straight away.

use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use tokio::sync::{Semaphore, watch};

use crate::state::lock;

/// Checks that may run at once (each on a blocking thread).
pub const MAX_STUCK_CHECKS: usize = 64;

type Answer = Arc<dyn Any + Send + Sync>;
type Flights = HashMap<(&'static str, PathBuf), watch::Receiver<Option<Answer>>>;

static FLIGHTS: LazyLock<Mutex<Flights>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static PERMITS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_STUCK_CHECKS)));

/// Run `work` (blocking filesystem calls on `path`) on a blocking thread and
/// wait up to `timeout` for its answer. `None` when it didn't answer in
/// time (the folder isn't responding). `kind` names the check, so different
/// checks of one path don't share answers.
pub async fn guarded<T>(
    kind: &'static str,
    path: &Path,
    timeout: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T>
where
    T: Clone + Send + Sync + 'static,
{
    let key = (kind, path.to_path_buf());
    let rx = {
        let mut flights = lock(&FLIGHTS);
        match flights.get(&key) {
            Some(rx) => rx.clone(),
            None => {
                let Ok(permit) = Arc::clone(&PERMITS).try_acquire_owned() else {
                    tracing::debug!(path = %path.display(), "too many folder checks are stuck");
                    return None;
                };
                let (tx, rx) = watch::channel(None);
                flights.insert(key.clone(), rx.clone());
                tokio::task::spawn_blocking(move || {
                    let answer: Answer = Arc::new(work());
                    // Forget the flight first: a caller arriving after this
                    // starts a fresh check, one arriving before gets this
                    // answer.
                    lock(&FLIGHTS).remove(&key);
                    let _ = tx.send(Some(answer));
                    drop(permit);
                });
                rx
            }
        }
    };
    wait(rx, timeout).await
}

async fn wait<T: Clone + 'static>(
    mut rx: watch::Receiver<Option<Answer>>,
    timeout: Duration,
) -> Option<T> {
    let answered = tokio::time::timeout(timeout, rx.wait_for(Option::is_some)).await;
    match answered {
        Ok(Ok(answer)) => answer
            .as_ref()
            .and_then(|a| a.downcast_ref::<T>())
            .cloned(),
        // Timed out, or the check panicked (its sender is gone).
        _ => None,
    }
}

/// The metadata of `path`, or `None` when it didn't answer within
/// `timeout`. The error, when there is one, keeps its kind.
pub async fn metadata(
    path: &Path,
    timeout: Duration,
) -> Option<Result<std::fs::Metadata, std::io::ErrorKind>> {
    let p = path.to_path_buf();
    guarded("metadata", path, timeout, move || {
        std::fs::metadata(&p).map_err(|e| e.kind())
    })
    .await
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
            assert_eq!(ask.await.unwrap(), None, "not answered in time");
        }
        assert_eq!(STARTED.load(Ordering::SeqCst), 1);

        // Once the folder answers, the next check runs and answers (the
        // first ask may still get the stuck check's late answer).
        release.wait();
        let mut answer = None;
        for _ in 0..50 {
            answer = guarded("test", path, Duration::from_millis(500), || 8u32).await;
            if answer == Some(8) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(answer, Some(8));
        assert_eq!(STARTED.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn answers_come_back_with_their_error_kind() {
        let dir = tempfile::tempdir().unwrap();
        let m = metadata(dir.path(), Duration::from_secs(5)).await.unwrap();
        assert!(m.unwrap().is_dir());
        let missing = dir.path().join("missing");
        let m = metadata(&missing, Duration::from_secs(5)).await.unwrap();
        assert_eq!(m.unwrap_err(), std::io::ErrorKind::NotFound);
    }
}
