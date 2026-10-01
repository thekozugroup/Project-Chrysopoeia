//! Filesystem calls on folders that may stop answering.
//!
//! On a network share whose server went away (an NFS hard mount) or a stuck
//! FUSE mount, every system call on the folder blocks, sometimes for good.
//! `tokio::fs` runs each call on a blocking thread, and giving up on it with
//! a timeout leaves that thread stuck; repeated on every page load or every
//! retry of a job, stuck threads pile up until the runtime's blocking pool
//! (512 threads) is used up and everything that touches a disk stops,
//! healthy folders included.
//!
//! [`guarded`] runs one check per path (and kind of check) at a time: a
//! caller that comes while the same check is still running waits for its
//! answer, up to its own timeout, instead of starting another. At most
//! [`MAX_STUCK_CHECKS`] checks run at once; beyond that a check counts as
//! not answering straight away.
//!
//! The server and the worker share these checks, so a share that stopped
//! answering costs one thread per thing looked at, however often it is
//! looked at.

use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{Semaphore, watch};

/// Checks that may run at once (each on a blocking thread).
pub const MAX_STUCK_CHECKS: usize = 64;

/// How long a file or folder a job works with may take to answer before
/// it counts as not responding. Generous: an array disk that has to spin
/// up first takes several seconds.
pub const FOLDER_CHECK_TIMEOUT: Duration = if cfg!(any(test, feature = "test-hooks")) {
    Duration::from_secs(2)
} else {
    Duration::from_secs(30)
};

/// How often the files and folders of a long step (ffprobe, an encode, the
/// checks, putting the new file in place) are looked at while it runs. A
/// share that stops answering is noticed within this plus
/// [`FOLDER_CHECK_TIMEOUT`]; one look every few seconds at a disk that is
/// being read anyway costs nothing.
pub const WATCH_INTERVAL: Duration = if cfg!(any(test, feature = "test-hooks")) {
    Duration::from_millis(200)
} else {
    Duration::from_secs(5)
};

/// A file or folder that didn't answer in time: it is on a share or drive
/// that stopped responding.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{} isn't responding", path.display())]
pub struct NotAnswering {
    /// What didn't answer.
    pub path: PathBuf,
}

impl NotAnswering {
    /// `path` didn't answer.
    pub fn at(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }
}

type Answer = Arc<dyn Any + Send + Sync>;
type Flights = HashMap<(&'static str, PathBuf), watch::Receiver<Option<Answer>>>;

static FLIGHTS: LazyLock<Mutex<Flights>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static PERMITS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_STUCK_CHECKS)));

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a [`guarded`] check ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Checked<T> {
    Answered(T),
    /// No answer within the timeout.
    TimedOut,
    /// Too many checks are stuck already; this one didn't start.
    Overloaded,
}

/// Run `work` (blocking filesystem calls on `path`) on a blocking thread and
/// wait up to `timeout` for its answer. `None` when it didn't answer in
/// time (the folder isn't responding). `kind` names the check, so different
/// checks of one path don't share answers; one `kind` always answers with
/// the same type.
pub async fn guarded<T>(
    kind: &'static str,
    path: &Path,
    timeout: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T>
where
    T: Clone + Send + Sync + 'static,
{
    match checked(kind, path, timeout, work).await {
        Checked::Answered(answer) => Some(answer),
        Checked::TimedOut | Checked::Overloaded => None,
    }
}

async fn checked<T>(
    kind: &'static str,
    path: &Path,
    timeout: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Checked<T>
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
                    return Checked::Overloaded;
                };
                let (tx, rx) = watch::channel(None);
                flights.insert(key.clone(), rx.clone());
                tokio::task::spawn_blocking(move || {
                    hang::wait_while_hung(&key.1);
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
    let mut rx = rx;
    let answered = tokio::time::timeout(timeout, rx.wait_for(Option::is_some)).await;
    match answered {
        Ok(Ok(answer)) => answer
            .as_ref()
            .and_then(|a| a.downcast_ref::<T>())
            .cloned()
            .map_or(Checked::TimedOut, Checked::Answered),
        // Timed out, or the check panicked (its sender is gone).
        _ => Checked::TimedOut,
    }
}

/// The metadata of `path` (following links), or `None` when it didn't
/// answer within `timeout`. The error, when there is one, keeps its kind.
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

/// The metadata of `path` itself (not following a link), or `None` when it
/// didn't answer within `timeout`.
pub async fn symlink_metadata(
    path: &Path,
    timeout: Duration,
) -> Option<Result<std::fs::Metadata, std::io::ErrorKind>> {
    let p = path.to_path_buf();
    guarded("symlink_metadata", path, timeout, move || {
        std::fs::symlink_metadata(&p).map_err(|e| e.kind())
    })
    .await
}

/// Wait until one of `paths` stops answering, and return it. Each is looked
/// at (its metadata) now and then every `every`; one that gives no answer
/// within `timeout` hasn't answered. Any answer counts, a missing file
/// included. Never returns while they all answer: meant to run next to a
/// long step (`tokio::select!`), which is stopped when this returns.
pub async fn first_unanswered(paths: &[PathBuf], every: Duration, timeout: Duration) -> PathBuf {
    loop {
        for p in paths {
            let looked = checked("watch", p, timeout, {
                let p = p.clone();
                move || std::fs::metadata(&p).map(|_| ()).map_err(|e| e.kind())
            })
            .await;
            match looked {
                Checked::Answered(_) => {}
                Checked::TimedOut => return p.clone(),
                // Other shares are stuck; that says nothing about this one.
                Checked::Overloaded => {}
            }
        }
        tokio::time::sleep(every).await;
    }
}

/// Tests stand in for a share that stopped answering: every [`guarded`]
/// check of a path under one marked hung blocks until the mark is lifted.
#[cfg(any(test, feature = "test-hooks"))]
pub mod hang {
    use std::path::{Path, PathBuf};
    use std::sync::{LazyLock, Mutex};
    use std::time::Duration;

    use super::lock;

    static HUNG: LazyLock<Mutex<Vec<PathBuf>>> = LazyLock::new(Mutex::default);

    /// Make every check of `path`, and of anything under it, hang until
    /// the returned guard is dropped (even by a failing test, whose runtime
    /// would otherwise wait for the stuck thread forever).
    pub fn hang(path: &Path) -> Hung {
        lock(&HUNG).push(path.to_path_buf());
        Hung(path.to_path_buf())
    }

    /// Lets the path answer again when dropped.
    #[derive(Debug)]
    pub struct Hung(PathBuf);

    impl Drop for Hung {
        fn drop(&mut self) {
            let mut hung = lock(&HUNG);
            if let Some(i) = hung.iter().position(|p| *p == self.0) {
                hung.remove(i);
            }
        }
    }

    /// Whether `path` is (under) a path marked hung.
    pub fn is_hung(path: &Path) -> bool {
        lock(&HUNG).iter().any(|p| path.starts_with(p))
    }

    /// Block while `path` is marked hung.
    pub fn wait_while_hung(path: &Path) {
        while is_hung(path) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(not(any(test, feature = "test-hooks")))]
mod hang {
    use std::path::Path;

    #[inline]
    pub(super) fn wait_while_hung(_: &Path) {}
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
        for _ in 0..500 {
            answer = guarded("test", path, Duration::from_secs(5), || 8u32).await;
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
        let m = metadata(dir.path(), Duration::from_secs(30)).await.unwrap();
        assert!(m.unwrap().is_dir());
        let missing = dir.path().join("missing");
        let m = metadata(&missing, Duration::from_secs(30)).await.unwrap();
        assert_eq!(m.unwrap_err(), std::io::ErrorKind::NotFound);
        let m = symlink_metadata(&missing, Duration::from_secs(30))
            .await
            .unwrap();
        assert_eq!(m.unwrap_err(), std::io::ErrorKind::NotFound);
    }

    /// The watch over a long step returns the path that stopped answering,
    /// and nothing while they answer (a missing file is an answer).
    #[tokio::test]
    async fn the_watch_names_what_stopped_answering() {
        let dir = tempfile::tempdir().unwrap();
        let fine = dir.path().join("fine");
        let gone = dir.path().join("gone");
        let share = dir.path().join("share");
        std::fs::create_dir(&fine).unwrap();
        std::fs::create_dir(&share).unwrap();
        let paths = vec![fine.clone(), gone.clone(), share.join("film.mkv")];
        let quiet = tokio::time::timeout(
            Duration::from_millis(500),
            first_unanswered(&paths, Duration::from_millis(20), Duration::from_secs(30)),
        )
        .await;
        assert!(quiet.is_err(), "everything answers: {quiet:?}");

        let _hung = hang::hang(&share);
        let stuck = tokio::time::timeout(
            Duration::from_secs(60),
            first_unanswered(
                &paths,
                Duration::from_millis(20),
                Duration::from_millis(300),
            ),
        )
        .await
        .expect("the hung share is noticed");
        assert_eq!(stuck, share.join("film.mkv"));
    }
}
