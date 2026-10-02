//! One share that stopped answering can't use up the checks of other
//! folders: checks are counted by the mount they are on, a mount holds only
//! a few stuck checks, and a check that can't start because too many are
//! stuck elsewhere is "unknown, try again shortly", never "not answering",
//! so a healthy library is never taken for offline because of another.
//!
//! `fs_guard::hang` stands in for the share (every check under it blocks
//! until it is lifted), `fs_guard::hang::busy` for checks that find no room
//! to start. Tests count checks by a path's first folders, so each test's
//! libraries are mounts of their own.

use std::path::Path;
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

async fn browse(app: &TestApp, folder: &Path) -> crate::tests::support::Resp {
    app.get(&format!(
        "/api/fs/browse?path={}",
        urlencoding(folder.to_str().unwrap())
    ))
    .await
}

fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Two libraries, `Share` and `Healthy`, one file each, the queue paused.
async fn two_libraries() -> (TestApp, Value, Value) {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Share/stuck.mkv", h264());
    app.write("Healthy/fine.mkv", h264());
    let share = app.add_library("Share", json!({})).await;
    let healthy = app.add_library("Healthy", json!({})).await;
    (app, share, healthy)
}

/// Seventy folder-picker looks at different folders on a share that
/// stopped answering: they hold a few threads at most, the share answers
/// "isn't responding" at once after that, and the other library's folder
/// still opens, isn't shown as unreachable, and its queue moves. Before,
/// they used up every check: the healthy folder answered 503 and its
/// library was taken for offline.
#[tokio::test]
async fn one_hung_share_leaves_other_libraries_alone() {
    let (app, share, healthy) = two_libraries().await;
    let share_dir = app.media.join("Share");
    let healthy_dir = app.media.join("Healthy");
    for i in 0..70 {
        std::fs::create_dir(share_dir.join(format!("folder {i}"))).unwrap();
    }
    let hung = hang::hang(&share_dir);
    let looks: Vec<_> = (0..70)
        .map(|i| {
            let router = app.router.clone();
            let path = share_dir.join(format!("folder {i}"));
            tokio::spawn(async move {
                use tower::ServiceExt;
                let req = axum::http::Request::builder()
                    .uri(format!(
                        "/api/fs/browse?path={}",
                        urlencoding(path.to_str().unwrap())
                    ))
                    .body(axum::body::Body::empty())
                    .unwrap();
                router.oneshot(req).await.unwrap().status()
            })
        })
        .collect();
    for look in looks {
        assert_eq!(look.await.unwrap(), StatusCode::SERVICE_UNAVAILABLE);
    }
    assert!(
        hang::running_checks_on(&share_dir).await <= MAX_STUCK_PER_MOUNT,
        "{} checks stuck on the share",
        hang::running_checks_on(&share_dir).await
    );

    // Once its checks are stuck, the share says so at once.
    let asked = Instant::now();
    let r = browse(&app, &share_dir.join("folder 0")).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(r.json["code"], "not_responding", "{}", r.text);
    assert!(
        asked.elapsed() < Duration::from_secs(1),
        "{:?}",
        asked.elapsed()
    );

    // The healthy folder opens, and its library isn't unreachable.
    let r = browse(&app, &healthy_dir).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let lib = app
        .get(&format!(
            "/api/libraries/{}",
            healthy["id"].as_str().unwrap()
        ))
        .await
        .json;
    assert!(lib["path_error"].is_null(), "{lib}");
    let lib = app
        .get(&format!("/api/libraries/{}", share["id"].as_str().unwrap()))
        .await
        .json;
    assert!(
        lib["path_error"]
            .as_str()
            .is_some_and(|e| e.contains("isn't responding")),
        "{lib}"
    );

    // Its queue moves while the share hangs.
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
    drop(hung);
}

/// Checks that find no room to start (too many stuck elsewhere) say
/// nothing about their folder: the library isn't shown as unreachable,
/// the folder picker says to try again (not that the folder isn't
/// responding), and the library's jobs wait and are tried again shortly,
/// its library never taken for offline.
#[tokio::test]
async fn checks_that_cannot_start_never_take_a_library_offline() {
    let (app, _share, healthy) = two_libraries().await;
    let healthy_dir = app.media.join("Healthy");
    let busy = hang::busy(&healthy_dir);

    let lib = app
        .get(&format!(
            "/api/libraries/{}",
            healthy["id"].as_str().unwrap()
        ))
        .await
        .json;
    assert!(lib["path_error"].is_null(), "{lib}");
    let r = browse(&app, &healthy_dir).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(r.json["code"], "busy", "{}", r.text);
    assert!(!r.text.contains("isn't responding"), "{}", r.text);

    app.resume().await;
    // The other library's file converts; this one's waits, not failed and
    // its library not offline.
    wait("the other library's file to be converted", async || {
        app.fake.started().iter().any(|n| n == "stuck.mkv")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!app.fake.started().iter().any(|n| n == "fine.mkv"));
    assert!(
        app.state
            .dispatcher
            .offline_reason(uuid(&healthy["id"]))
            .is_none()
    );
    let failed = app.get("/api/files?status=failed").await;
    assert_eq!(failed.json["total"], 0, "{}", failed.json);
    let feed = app.get("/api/activity").await.text;
    assert!(!feed.contains("can't be reached"), "{feed}");

    drop(busy);
    wait(
        "the file to be converted once checks can start",
        async || app.fake.started().iter().any(|n| n == "fine.mkv"),
    )
    .await;
}

/// A share mounted after the list of mounts was read, that stops answering
/// moments later: its stuck checks are first counted with the mount above
/// it, where a healthy library is too. That library still opens in the
/// folder picker, isn't shown as unreachable, and its queue moves while
/// the share hangs; once the share is known as a mount, its stuck checks
/// are counted by it. Before, they stayed counted against the mount above
/// for as long as the share hung: the healthy folder answered 503 "isn't
/// responding" at once, and its library's queue stopped.
#[tokio::test]
async fn a_share_mounted_later_doesnt_stop_the_mount_it_is_on() {
    let (app, share, healthy) = two_libraries().await;
    // Both libraries are on one mount (tests make their mounts up); the
    // share isn't known as a mount of its own yet.
    let _parent = hang::mount(&app.media);
    let share_dir = app.media.join("Share");
    let healthy_dir = app.media.join("Healthy");
    for i in 0..12 {
        std::fs::create_dir(share_dir.join(format!("folder {i}"))).unwrap();
    }
    let hung = hang::hang(&share_dir);
    let looks: Vec<_> = (0..12)
        .map(|i| {
            let router = app.router.clone();
            let path = share_dir.join(format!("folder {i}"));
            tokio::spawn(async move {
                use tower::ServiceExt;
                let req = axum::http::Request::builder()
                    .uri(format!(
                        "/api/fs/browse?path={}",
                        urlencoding(path.to_str().unwrap())
                    ))
                    .body(axum::body::Body::empty())
                    .unwrap();
                router.oneshot(req).await.unwrap().status()
            })
        })
        .collect();
    for look in looks {
        assert_eq!(look.await.unwrap(), StatusCode::SERVICE_UNAVAILABLE);
    }

    // The healthy folder opens, and its library isn't unreachable.
    let r = browse(&app, &healthy_dir).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let lib = app
        .get(&format!(
            "/api/libraries/{}",
            healthy["id"].as_str().unwrap()
        ))
        .await
        .json;
    assert!(lib["path_error"].is_null(), "{lib}");
    // The share says it isn't responding.
    let r = browse(&app, &share_dir.join("folder 0")).await;
    assert_eq!(r.json["code"], "not_responding", "{}", r.text);

    // Its queue moves while the share hangs.
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

    // The share turns out to be a mount of its own: its stuck checks are
    // counted by it, and none by the mount above.
    let _mounted = hang::mount(&share_dir);
    assert!(hang::running_checks_on(&share_dir).await <= MAX_STUCK_PER_MOUNT);
    wait("the mount above to count no stuck checks", async || {
        hang::running_checks_on(&app.media).await == 0
    })
    .await;
    let r = browse(&app, &healthy_dir).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    drop(hung);
    let lib = app
        .get(&format!("/api/libraries/{}", share["id"].as_str().unwrap()))
        .await;
    assert_eq!(lib.status, StatusCode::OK, "{}", lib.text);
}
