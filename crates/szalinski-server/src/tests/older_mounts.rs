//! Mount points remembered by an older version (schema v12), which didn't
//! note what was mounted there.
//!
//! What is mounted at such a place is noted at the first look that finds
//! something mounted there: at start-up, and every few seconds after while
//! any are left (`share_mounts::note_unknown`), not only when a job or a
//! view happens to look at the folder, so another drive put there later is
//! told apart. The folder below bound onto itself (what a Docker bind mount
//! shows when the container started before the share was mounted, and both
//! are on one disk) is never noted as the share, and with nothing mounted
//! there the folder is not connected.
//!
//! `fs_guard::hang::mount` stands in for mounting the share,
//! `hang::mount_other` for another filesystem, `hang::mount_disk` for the
//! disk the test's folders are on and `hang::bind_folder_below` for the
//! bare folder bound onto itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

use super::support::*;
use crate::db;
use crate::services::fs_guard::hang;

/// Far more than anything here should take, even on a loaded machine.
const PATIENCE: Duration = Duration::from_secs(120);

async fn wait(what: &str, f: impl AsyncFn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !f().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn uuid(id: &Value) -> Uuid {
    Uuid::parse_str(id.as_str().unwrap()).unwrap()
}

/// The names in a folder, sorted (hidden files included).
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

async fn path_error(app: &TestApp, lib: &Value) -> Option<String> {
    app.get(&format!("/api/libraries/{}", lib["id"].as_str().unwrap()))
        .await
        .json["path_error"]
        .as_str()
        .map(str::to_string)
}

async fn done(app: &TestApp) -> Value {
    app.get("/api/files?status=done").await.json["total"].clone()
}

/// What is remembered for `folder` at `mount` (`None` inside: nothing
/// noted about what is mounted there).
async fn noted(app: &TestApp, folder: &Path, mount: &Path) -> Option<Option<String>> {
    db::folder_mounts::get(app.state.db.pool(), folder.to_str().unwrap())
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.mount == mount.to_str().unwrap())
        .map(|m| m.identity.map(|i| i.to_string()))
}

/// Folder mode with the output folder on a share, one file converted into
/// it; then the server is stopped (the queue paused) and the database made
/// to look like an older version's: the mount points remembered, nothing
/// noted about what was mounted there.
async fn older_install(app: TestApp) -> (TempDir, Value, PathBuf, hang::Marked) {
    app.pause().await;
    let out = app.dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let mounted = hang::mount(&out);
    let r = app
        .patch(
            "/api/settings",
            json!({ "output_mode": "folder", "output_folder": out.to_str().unwrap() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.write("Movies/film.mp4", h264());
    let lib = app.add_library("Movies", json!({})).await;
    app.resume().await;
    wait("the first file to be converted", async || {
        done(&app).await == 1
    })
    .await;
    app.pause().await;
    assert!(
        noted(&app, &out, &out).await.is_some(),
        "the share is remembered"
    );
    let dir = app.stop().await;
    let path = dir.path().join("data").join(db::DB_FILE_NAME);
    let old = db::Db::open(&path).await.unwrap();
    sqlx::query("UPDATE folder_mounts SET fstype = NULL, source = NULL, root = NULL")
        .execute(old.pool())
        .await
        .unwrap();
    old.pool().close().await;
    (dir, lib, out, mounted)
}

async fn restart(dir: TempDir) -> TestApp {
    TestApp::start(TestOptions {
        dir: Some(dir),
        fake: Arc::new(FakeToolkit::default()),
        ..TestOptions::default()
    })
    .await
}

/// Started with the share mounted, what is mounted there is noted at once,
/// before any job or view looks at the output folder; a tmpfs mounted in
/// the share's place later is then another drive: the next job waits and
/// nothing is written there. Before, nothing was noted while the queue was
/// idle, and the tmpfs found at the next job's look was noted as the share
/// and written into.
#[tokio::test]
async fn an_older_versions_share_is_noted_at_start_up() {
    let (dir, lib, out, mounted) = older_install(TestApp::new().await).await;
    let app = restart(dir).await;
    assert_eq!(
        noted(&app, &out, &out).await,
        Some(Some(hang::share_at(&out).to_string())),
        "noted at start-up"
    );

    drop(mounted);
    let _other = hang::mount_other(&out, "tmpfs");
    app.write("Movies/later.mp4", h264());
    app.rescan(lib["id"].as_str().unwrap()).await;
    app.resume().await;
    wait("the library to wait for the share", async || {
        path_error(&app, &lib)
            .await
            .is_some_and(|e| e.contains("A different drive is mounted at"))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(app.fake.started().is_empty(), "the job waits");
    assert_eq!(names(&out), ["film.mkv"], "nothing written there");
}

/// Started with the bare folder bound onto itself in the share's place
/// (the container started before the host mounted the share): that is
/// never noted as the share. The next job waits with "A different drive is
/// mounted at …" and writes nothing into the bare folder; once the share
/// is mounted over it, the share is noted and the job converts onto it.
/// Before, the bare folder was noted as the share and written into.
#[tokio::test]
async fn the_bare_folder_bound_onto_itself_is_never_noted_as_the_share() {
    let app = TestApp::new().await;
    let _disk = hang::mount_disk(app.dir.path());
    let (dir, lib, out, mounted) = older_install(app).await;
    drop(mounted);
    let shared = out.with_extension("share");
    std::fs::rename(&out, &shared).unwrap();
    std::fs::create_dir(&out).unwrap();
    let _bound = hang::bind_folder_below(&out);

    let app = restart(dir).await;
    assert_eq!(noted(&app, &out, &out).await, Some(None), "not noted");
    app.write("Movies/later.mp4", h264());
    app.rescan(lib["id"].as_str().unwrap()).await;
    app.resume().await;
    wait("the library to wait for the share", async || {
        path_error(&app, &lib).await.is_some_and(|e| {
            e.contains(&format!(
                "A different drive is mounted at {}",
                out.display()
            ))
        })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(app.fake.started().is_empty(), "the job waits");
    assert!(names(&out).is_empty(), "nothing written: {:?}", names(&out));
    assert_eq!(noted(&app, &out, &out).await, Some(None), "still not noted");

    // The host mounts the share; it shows on top of the bound folder.
    std::fs::remove_dir(&out).unwrap();
    std::fs::rename(&shared, &out).unwrap();
    let _share = hang::mount(&out);
    app.state.dispatcher.recheck_now(uuid(&lib["id"]));
    wait("the file to be converted onto the share", async || {
        done(&app).await == 2
    })
    .await;
    assert_eq!(
        noted(&app, &out, &out).await,
        Some(Some(hang::share_at(&out).to_string()))
    );
    assert_eq!(names(&out), ["film.mkv", "later.mkv"]);
    assert_eq!(path_error(&app, &lib).await, None);
}

/// Started with nothing mounted at the share's place: the output folder is
/// not connected (nothing is learned from the bare mount point), and the
/// share is noted promptly once it is mounted again, while the queue is
/// paused and nothing looks at the folder; another drive put there after
/// that is told apart.
#[tokio::test]
async fn an_older_versions_share_is_noted_once_it_is_back() {
    let (dir, lib, out, mounted) = older_install(TestApp::new().await).await;
    drop(mounted);
    let shared = out.with_extension("share");
    std::fs::rename(&out, &shared).unwrap();
    std::fs::create_dir(&out).unwrap();

    let app = restart(dir).await;
    let r = app.get("/api/settings/folders").await;
    let output = r
        .json
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["setting"] == "output_folder")
        .cloned();
    let output = output.unwrap_or_else(|| panic!("{}", r.json));
    assert_eq!(
        output["problem"],
        format!(
            "The drive or share mounted at {} isn't connected. Reconnect it, and its \
             conversions continue.",
            out.display()
        ),
        "{output}"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(noted(&app, &out, &out).await, Some(None), "nothing noted");

    // Back, with nothing else looking at the folder: noted promptly.
    std::fs::remove_dir(&out).unwrap();
    std::fs::rename(&shared, &out).unwrap();
    let mounted = hang::mount(&out);
    wait("the share to be noted", async || {
        noted(&app, &out, &out).await == Some(Some(hang::share_at(&out).to_string()))
    })
    .await;

    // Another drive in its place now is told apart.
    drop(mounted);
    let _other = hang::mount_other(&out, "tmpfs");
    app.write("Movies/later.mp4", h264());
    app.rescan(lib["id"].as_str().unwrap()).await;
    app.resume().await;
    wait("the library to wait for the share", async || {
        path_error(&app, &lib)
            .await
            .is_some_and(|e| e.contains("A different drive is mounted at"))
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(app.fake.started().is_empty(), "the job waits");
    assert_eq!(names(&out), ["film.mkv"], "nothing written there");
}
