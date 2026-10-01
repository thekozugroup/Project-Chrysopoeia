//! Shares that are no longer mounted, and shares that hang and are then
//! unmounted.
//!
//! An unmounted share leaves its mount point behind as an ordinary folder.
//! The drives and shares a library folder, the output folder and the work
//! folder were seen mounted from are remembered; while one isn't mounted,
//! its folder counts as not connected: nothing is written there, and the
//! jobs that need it wait. A share that hangs and is then unmounted lazily
//! (`umount -l`) leaves its stuck checks behind; they are never counted
//! against the mount it was on.
//!
//! `fs_guard::hang::mount` stands in for mounting a share (dropping its
//! guard unmounts it) and `fs_guard::hang::hang` for a share that stopped
//! answering.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

use super::support::*;
use crate::services::fs_guard::{MAX_STUCK_PER_MOUNT, hang};

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

async fn library(app: &TestApp, lib: &Value) -> Value {
    app.get(&format!("/api/libraries/{}", lib["id"].as_str().unwrap()))
        .await
        .json
}

async fn path_error(app: &TestApp, lib: &Value) -> Option<String> {
    library(app, lib).await["path_error"]
        .as_str()
        .map(str::to_string)
}

/// The folder picker's look at `folder`, from its own task.
fn browse_in_background(app: &TestApp, folder: &Path) -> tokio::task::JoinHandle<StatusCode> {
    use tower::ServiceExt;
    let router = app.router.clone();
    let uri = format!(
        "/api/fs/browse?path={}",
        url_escape(folder.to_str().unwrap())
    );
    tokio::spawn(async move {
        let req = axum::http::Request::builder()
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap();
        router.oneshot(req).await.unwrap().status()
    })
}

fn url_escape(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Two shares on the mount of a healthy library stop answering, with
/// twice their room of stuck checks on one of them (folder-picker looks in
/// two of its folders), and are unmounted lazily while they hang. The
/// healthy library still opens, isn't shown as unreachable, and its queue
/// moves. Before, the stuck checks were counted by the mount above once
/// the shares were gone from the list of mounts: the healthy library was
/// "isn't responding" and its queue stopped for as long as they hung.
#[tokio::test]
async fn shares_unmounted_while_they_hang_leave_the_mount_they_were_on_alone() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Healthy/fine.mkv", h264());
    let healthy = app.add_library("Healthy", json!({})).await;
    let healthy_dir = app.media.join("Healthy");
    let _parent = hang::mount(&app.media);
    let shares = [app.media.join("NasA"), app.media.join("NasB")];
    let mut mounted = Vec::new();
    let mut hung = Vec::new();
    for share in &shares {
        std::fs::create_dir_all(share).unwrap();
        mounted.push(hang::mount(share));
        hung.push(hang::hang(share));
    }
    let folders: [(&Path, &[&str]); 2] = [(&shares[0], &["one", "two"]), (&shares[1], &["one"])];
    for (share, subfolders) in folders {
        for sub in subfolders {
            let looks: Vec<_> = (0..12)
                .map(|i| browse_in_background(&app, &share.join(sub).join(format!("{i}"))))
                .collect();
            for look in looks {
                assert_eq!(look.await.unwrap(), StatusCode::SERVICE_UNAVAILABLE);
            }
        }
    }
    let stuck = hang::running_checks_on(&shares[0]).await;
    assert!(
        stuck > MAX_STUCK_PER_MOUNT,
        "{stuck} checks stuck on the share"
    );
    // (A healthy check of the library may be under way for a moment.)
    wait(
        "no stuck check to be counted by the mount above",
        async || hang::running_checks_on(&app.media).await == 0,
    )
    .await;

    // Unmounted while they hang (`umount -l`): their checks are never
    // counted by the mount they were on.
    mounted.clear();
    wait(
        "no stuck check to be counted by the mount above",
        async || hang::running_checks_on(&app.media).await == 0,
    )
    .await;
    let asked = Instant::now();
    let r = app
        .get(&format!(
            "/api/fs/browse?path={}",
            url_escape(healthy_dir.to_str().unwrap())
        ))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(
        asked.elapsed() < Duration::from_secs(10),
        "{:?}",
        asked.elapsed()
    );
    assert_eq!(path_error(&app, &healthy).await, None);

    app.resume().await;
    wait("the healthy library's file to be converted", async || {
        app.fake.started().iter().any(|n| n == "fine.mkv")
            && app.get("/api/files?status=done").await.json["total"] == 1
    })
    .await;
    assert!(
        app.state
            .dispatcher
            .offline_reason(uuid(&healthy["id"]))
            .is_none()
    );
    assert_eq!(path_error(&app, &healthy).await, None);
    drop(hung);
}

/// Folder mode with the output folder on a share: set it up, with one file
/// in `Movies` and the queue paused. Returns the output folder and its
/// mount.
async fn output_on_share(app: &TestApp) -> (Value, PathBuf, hang::Marked) {
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
    (lib, out, mounted)
}

/// The output folder is a share that was seen mounted and isn't any more
/// (unmounted cleanly while the server ran, between two jobs): the next
/// job isn't converted into the bare mount point. It waits with its
/// library "not connected", and converts once the share is back; the
/// worker is told which mounts its folders are on, to check again before
/// it writes anything. Before, the job converted into the mount point on
/// the disk below, where the file was hidden once the share was mounted
/// again.
#[tokio::test]
async fn nothing_is_written_into_an_unmounted_output_share() {
    let app = TestApp::new().await;
    let (lib, out, mounted) = output_on_share(&app).await;
    app.resume().await;
    wait("the first file to be converted", async || {
        app.get("/api/files?status=done").await.json["total"] == 1
    })
    .await;
    let specs = app.fake.run_specs.lock().unwrap().clone();
    assert!(
        specs.iter().all(|s| s.mounts.contains(&out)),
        "the worker checks the output share: {:?}",
        specs.iter().map(|s| &s.mounts).collect::<Vec<_>>()
    );
    app.pause().await;

    // Unmounted cleanly: its mount point is an empty folder.
    drop(mounted);
    let away = out.with_extension("away");
    std::fs::rename(&out, &away).unwrap();
    std::fs::create_dir(&out).unwrap();
    app.write("Movies/later.mp4", h264());
    app.rescan(lib["id"].as_str().unwrap()).await;
    app.resume().await;
    wait("the library to wait for the share", async || {
        path_error(&app, &lib)
            .await
            .is_some_and(|e| e.contains("isn't connected"))
    })
    .await;
    let reason = path_error(&app, &lib).await.unwrap_or_default();
    assert!(
        reason.contains(&format!("mounted at {}", out.display())),
        "{reason}"
    );
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!app.fake.started().iter().any(|n| n == "later.mp4"));
    assert!(names(&out).is_empty(), "nothing written: {:?}", names(&out));
    let failed = app.get("/api/files?status=failed").await;
    assert_eq!(failed.json["total"], 0, "{}", failed.json);

    // Mounted again: the file converts into the share.
    std::fs::remove_dir(&out).unwrap();
    std::fs::rename(&away, &out).unwrap();
    let _mounted = hang::mount(&out);
    app.state.dispatcher.recheck_now(uuid(&lib["id"]));
    wait("the file to be converted", async || {
        app.fake.started().iter().any(|n| n == "later.mp4")
            && app.get("/api/files?status=done").await.json["total"] == 2
    })
    .await;
    assert_eq!(path_error(&app, &lib).await, None);
}

/// A share removed for good: the output folder moved off it in Settings
/// is used at once, and choosing the old folder again makes it an
/// ordinary folder (its mounts are learned afresh).
#[tokio::test]
async fn an_output_folder_moved_off_a_share_relearns_its_mounts() {
    let app = TestApp::new().await;
    let (lib, out, mounted) = output_on_share(&app).await;
    app.resume().await;
    wait("the file to be converted", async || {
        app.get("/api/files?status=done").await.json["total"] == 1
    })
    .await;
    app.pause().await;
    drop(mounted);
    app.write("Movies/later.mp4", h264());
    app.rescan(lib["id"].as_str().unwrap()).await;
    app.resume().await;
    wait("the library to wait for the share", async || {
        path_error(&app, &lib)
            .await
            .is_some_and(|e| e.contains("isn't connected"))
    })
    .await;

    // Moved off the share.
    let local = app.dir.path().join("local");
    std::fs::create_dir_all(&local).unwrap();
    let r = app
        .patch(
            "/api/settings",
            json!({ "output_folder": local.to_str().unwrap() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.state.dispatcher.recheck_now(uuid(&lib["id"]));
    wait("the file to be converted into the new folder", async || {
        app.get("/api/files?status=done").await.json["total"] == 2
    })
    .await;
    assert_eq!(path_error(&app, &lib).await, None);

    // The old folder, chosen again, is an ordinary folder now.
    let r = app
        .patch(
            "/api/settings",
            json!({ "output_folder": out.to_str().unwrap() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.write("Movies/third.mp4", h264());
    app.rescan(lib["id"].as_str().unwrap()).await;
    wait("the third file to be converted", async || {
        app.get("/api/files?status=done").await.json["total"] == 3
    })
    .await;
    assert_eq!(path_error(&app, &lib).await, None);
}

/// A library folder on a share that isn't mounted any more is "not
/// connected" even when its mount point holds files of its own: a scan
/// leaves the library's files listed, and its jobs wait. A library added
/// again (the share removed for good, its files now in that folder) learns
/// its mounts afresh.
#[tokio::test]
async fn a_library_on_an_unmounted_share_is_not_connected() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/film.mp4", h264());
    let root = std::fs::canonicalize(app.media.join("Movies")).unwrap();
    let mounted = hang::mount(&root);
    let lib = app.add_library("Movies", json!({})).await;
    assert_eq!(path_error(&app, &lib).await, None);

    // Unmounted cleanly; the folder below holds a file of its own.
    drop(mounted);
    let away = root.with_extension("away");
    std::fs::rename(&root, &away).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("README.txt"), "the disk below").unwrap();
    let reason = path_error(&app, &lib).await.unwrap_or_default();
    assert!(
        reason.contains(&format!(
            "The drive or share mounted at {} isn't connected",
            root.display()
        )),
        "{reason}"
    );
    app.rescan(lib["id"].as_str().unwrap()).await;
    let files = app.get("/api/files").await;
    assert_eq!(files.json["total"], 1, "still listed: {}", files.json);
    app.resume().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(app.fake.started().is_empty());
    assert_eq!(names(&root), ["README.txt"], "nothing written there");

    // Removed for good: the files are moved into the folder, and the
    // library is added again.
    std::fs::rename(away.join("film.mp4"), root.join("film.mp4")).unwrap();
    let r = app
        .delete(&format!("/api/libraries/{}", lib["id"].as_str().unwrap()))
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text);
    let lib = app.add_library("Movies", json!({})).await;
    assert_eq!(path_error(&app, &lib).await, None);
    wait("the file to be converted", async || {
        app.fake.started().iter().any(|n| n == "film.mp4")
    })
    .await;
}
