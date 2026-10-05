//! Another filesystem mounted where a share was, folders given as links
//! to a share, and a hung share mounted again at its place.
//!
//! A share's mount point is remembered with what was mounted there (its
//! type, source and root, as `/proc/self/mountinfo` says), so a tmpfs, or
//! the bare folder bind-mounted onto itself (what a Docker bind mount shows
//! when the container started before the host mounted the share), isn't
//! taken for the share; the user can say to use the drive there now. A
//! folder given as a link is followed to the share it leads to. A share
//! that hung, was unmounted lazily (`umount -l`) and mounted again at the
//! same place is looked at afresh, not through the checks stuck on the old
//! mount.
//!
//! `fs_guard::hang::mount` stands in for mounting a share (dropping its
//! guard unmounts it), `hang::mount_other` for mounting another filesystem
//! there, and `hang::hang_mount` for a mount that stopped answering.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

use super::support::*;
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

async fn done(app: &TestApp) -> Value {
    app.get("/api/files?status=done").await.json["total"].clone()
}

/// Unmount the share at `folder` cleanly: its content goes away (until
/// [`mend`]), and an empty folder is left in its place.
fn unmount(folder: &Path, mounted: hang::Marked) -> PathBuf {
    drop(mounted);
    let away = folder.with_extension("away");
    std::fs::rename(folder, &away).unwrap();
    std::fs::create_dir(folder).unwrap();
    away
}

/// The share's content back at `folder` (mount it again with
/// `hang::mount`).
fn mend(folder: &Path, away: &Path) {
    std::fs::remove_dir_all(folder).unwrap();
    std::fs::rename(away, folder).unwrap();
}

/// Folder mode with the output folder on a share, a first file converted
/// into it; the queue is paused after. Returns the library, the output
/// folder and its mount.
async fn output_share_in_use(app: &TestApp) -> (Value, PathBuf, hang::Marked) {
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
        done(app).await == 1
    })
    .await;
    app.pause().await;
    (lib, out, mounted)
}

/// Another filesystem is mounted where the output share was (a tmpfs, the
/// bare folder bind-mounted onto itself): the next job isn't converted into
/// it. It waits with its library saying "A different drive is mounted at
/// …", which names that place (`changed_mount`), and nothing is written
/// there. Telling Szalinski to use the drive there now
/// (`POST /api/libraries/{id}/relearn-mounts`) lets the job convert into
/// it. Before, whatever was mounted at that place was taken for the share.
#[tokio::test]
async fn another_drive_at_the_output_share_waits_until_the_user_says_to_use_it() {
    let app = TestApp::new().await;
    let (lib, out, mounted) = output_share_in_use(&app).await;
    let lib_id = lib["id"].as_str().unwrap().to_string();
    // Nothing is different yet: nothing to take anew.
    let r = app
        .post_empty(&format!("/api/libraries/{lib_id}/relearn-mounts"))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["code"], "nothing_changed", "{}", r.json);

    // The share is unmounted, and the bare folder below it bind-mounted in
    // its place.
    let away = unmount(&out, mounted);
    let other = hang::mount_other(&out, "the bare folder");
    app.write("Movies/later.mp4", h264());
    app.rescan(&lib_id).await;
    app.resume().await;
    wait("the library to wait for the share", async || {
        path_error(&app, &lib)
            .await
            .is_some_and(|e| e.contains("A different drive is mounted at"))
    })
    .await;
    let view = library(&app, &lib).await;
    assert_eq!(
        view["path_error"],
        format!(
            "A different drive is mounted at {} than before. Reconnect the usual one, or tell \
             Szalinski to use the one there now.",
            out.display()
        ),
        "{view}"
    );
    assert_eq!(view["changed_mount"], out.to_str().unwrap(), "{view}");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!app.fake.started().iter().any(|n| n == "later.mp4"));
    assert!(names(&out).is_empty(), "nothing written: {:?}", names(&out));
    assert_eq!(app.get("/api/files?status=failed").await.json["total"], 0);

    // The user says to use the drive there now.
    let r = app
        .post_empty(&format!("/api/libraries/{lib_id}/relearn-mounts"))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    wait("the file to be converted into it", async || {
        app.fake.started().iter().any(|n| n == "later.mp4") && done(&app).await == 2
    })
    .await;
    let view = library(&app, &lib).await;
    assert_eq!(view["path_error"], Value::Null, "{view}");
    assert_eq!(view["changed_mount"], Value::Null, "{view}");
    assert_eq!(names(&out), ["later.mkv"]);
    let feed = app.get("/api/activity?limit=50").await.json.to_string();
    assert!(
        feed.contains(&format!(
            "Movies now uses the drive mounted at {}.",
            out.display()
        )),
        "{feed}"
    );
    drop(other);
    drop(away);
}

/// Saving Settings with the output folder as it was (picked again, or
/// sent with another output option) keeps the drive remembered for it:
/// another drive in the share's place is not taken for it, the job keeps
/// waiting, and nothing is written there. The Settings page's own action
/// (`POST /api/settings/relearn-mounts`) takes the drive there now, after
/// `GET /api/settings/folders` says what is wrong. Before, saving the same
/// folder again learned whatever was mounted there.
#[tokio::test]
async fn saving_the_output_folder_again_keeps_the_usual_drive() {
    let app = TestApp::new().await;
    let (lib, out, mounted) = output_share_in_use(&app).await;
    let _away = unmount(&out, mounted);
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
    // Another setting saved meanwhile, and the same folder saved again
    // (with another output option, and alone): nothing changes.
    for body in [
        json!({ "max_jobs": 2 }),
        json!({ "output_folder": out.to_str().unwrap(), "keep_file_dates": false }),
        json!({ "output_folder": out.to_str().unwrap() }),
    ] {
        let r = app.patch("/api/settings", body.clone()).await;
        assert_eq!(r.status, StatusCode::OK, "{body}: {}", r.text);
        app.state.dispatcher.recheck_now(uuid(&lib["id"]));
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            !app.fake.started().iter().any(|n| n == "later.mp4"),
            "{body}: the job waits"
        );
        assert!(names(&out).is_empty(), "{body}: {:?}", names(&out));
        assert_eq!(
            library(&app, &lib).await["changed_mount"],
            out.to_str().unwrap(),
            "{body}"
        );
    }

    // Settings says what is wrong with the output folder.
    let r = app.get("/api/settings/folders").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let output = r
        .json
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["setting"] == "output_folder")
        .cloned();
    let output = output.unwrap_or_else(|| panic!("{}", r.json));
    assert_eq!(output["path"], out.to_str().unwrap());
    assert_eq!(output["changed_mount"], out.to_str().unwrap());
    assert!(
        output["problem"]
            .as_str()
            .is_some_and(|p| p.contains("A different drive is mounted at")),
        "{output}"
    );

    // Its action takes the drive there now.
    let r = app.post_empty("/api/settings/relearn-mounts").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let output = r
        .json
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["setting"] == "output_folder")
        .cloned();
    let output = output.unwrap_or_else(|| panic!("{}", r.json));
    assert_eq!(output["problem"], Value::Null, "{output}");
    assert_eq!(output["changed_mount"], Value::Null, "{output}");
    wait("the file to be converted into it", async || {
        done(&app).await == 2
    })
    .await;
    assert_eq!(path_error(&app, &lib).await, None);
    assert_eq!(names(&out), ["later.mkv"]);
    let feed = app.get("/api/activity?limit=50").await.json.to_string();
    assert!(
        feed.contains(&format!(
            "The output folder {} now uses the drive mounted at {}.",
            out.display(),
            out.display()
        )),
        "{feed}"
    );
    // Nothing is different any more.
    let r = app.post_empty("/api/settings/relearn-mounts").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["code"], "nothing_changed", "{}", r.json);
}

/// The same with the share unmounted and nothing in its place: saving the
/// output folder again as it was, or given through a link to the same
/// place, doesn't make the bare mount point the output folder. The job
/// keeps waiting ("isn't connected"), nothing is written into the mount
/// point, and the Settings action has nothing to take (a place with
/// nothing mounted stays not connected). Once the share is back, the job
/// converts onto it. Before, the bare mount point was learned as an
/// ordinary folder and the new file written into it, hidden under the
/// share once it was mounted again.
#[tokio::test]
async fn saving_the_output_folder_again_while_its_share_is_away_waits_for_it() {
    let app = TestApp::new().await;
    let (lib, out, mounted) = output_share_in_use(&app).await;
    let away = unmount(&out, mounted);
    app.write("Movies/later.mp4", h264());
    app.rescan(lib["id"].as_str().unwrap()).await;
    app.resume().await;
    wait("the library to wait for the share", async || {
        path_error(&app, &lib)
            .await
            .is_some_and(|e| e.contains("isn't connected"))
    })
    .await;
    let link = app.dir.path().join("out-link");
    std::os::unix::fs::symlink(&out, &link).unwrap();
    for (folder, also) in [
        (&out, json!({ "keep_file_dates": false })),
        (&out, json!({})),
        (&link, json!({})),
    ] {
        let mut body = also.clone();
        body["output_folder"] = json!(folder.to_str().unwrap());
        let r = app.patch("/api/settings", body.clone()).await;
        assert_eq!(r.status, StatusCode::OK, "{body}: {}", r.text);
        app.state.dispatcher.recheck_now(uuid(&lib["id"]));
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            !app.fake.started().iter().any(|n| n == "later.mp4"),
            "{body}: the job waits"
        );
        assert!(names(&out).is_empty(), "{body}: {:?}", names(&out));
        let reason = path_error(&app, &lib).await.unwrap_or_default();
        assert!(
            reason.contains(&format!(
                "The drive or share mounted at {} isn't connected",
                out.display()
            )),
            "{body}: {reason}"
        );
    }
    // The link is the output folder now, with the share remembered for it.
    let known = crate::db::folder_mounts::get(app.state.db.pool(), link.to_str().unwrap())
        .await
        .unwrap();
    assert!(
        known
            .iter()
            .any(|m| m.mount == out.to_str().unwrap() && m.identity == Some(hang::share_at(&out))),
        "{known:?}"
    );
    let r = app.post_empty("/api/settings/relearn-mounts").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let r = app.get("/api/settings/folders").await;
    assert!(r.json.to_string().contains("isn't connected"), "{}", r.json);

    mend(&out, &away);
    let _mounted = hang::mount(&out);
    app.state.dispatcher.recheck_now(uuid(&lib["id"]));
    wait("the file to be converted onto the share", async || {
        done(&app).await == 2
    })
    .await;
    assert_eq!(names(&out), ["film.mkv", "later.mkv"]);
    assert_eq!(path_error(&app, &lib).await, None);
}

/// The work folder the server was started with (`TEMP_DIR`) is on a share
/// that is unmounted, and the user picks that same folder in Settings (from
/// Automatic to "A specific folder"): that is no change. The share stays
/// remembered for it, nothing is written into the bare mount point, and
/// the job keeps waiting until the share is back. Before, the setting was
/// taken for a new folder: it was written into to check it, what was
/// remembered for it was forgotten, and the job's work file went into the
/// bare mount point.
#[tokio::test]
async fn picking_the_started_with_work_folder_in_settings_keeps_its_share() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mounted = hang::mount(&work);
    let app = TestApp::start(TestOptions {
        dir: Some(dir),
        configure: Box::new(|c, root| c.temp_dir = Some(root.join("work"))),
        ..TestOptions::default()
    })
    .await;
    app.write("Movies/film.mp4", h264());
    let lib = app.add_library("Movies", json!({})).await;
    wait("the first file to be converted", async || {
        done(&app).await == 1
    })
    .await;
    app.pause().await;
    let work_key = work.to_str().unwrap();
    let known = crate::db::folder_mounts::get(app.state.db.pool(), work_key)
        .await
        .unwrap();
    assert!(
        known.iter().any(|m| m.mount == work_key),
        "the share is remembered for the work folder: {known:?}"
    );

    let away = unmount(&work, mounted);
    app.write("Movies/later.mp4", h264());
    app.rescan(lib["id"].as_str().unwrap()).await;
    app.resume().await;
    wait("the library to wait for the work folder", async || {
        path_error(&app, &lib)
            .await
            .is_some_and(|e| e.contains("isn't connected"))
    })
    .await;
    let r = app
        .patch("/api/settings", json!({ "temp_dir": work_key }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.state.dispatcher.recheck_now(uuid(&lib["id"]));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !app.fake.started().iter().any(|n| n == "later.mp4"),
        "the job waits"
    );
    assert!(
        names(&work).is_empty(),
        "nothing written: {:?}",
        names(&work)
    );
    let reason = path_error(&app, &lib).await.unwrap_or_default();
    assert!(reason.contains("isn't connected"), "{reason}");
    let known = crate::db::folder_mounts::get(app.state.db.pool(), work_key)
        .await
        .unwrap();
    assert!(
        known.iter().any(|m| m.mount == work_key),
        "still remembered: {known:?}"
    );

    mend(&work, &away);
    let _mounted = hang::mount(&work);
    app.state.dispatcher.recheck_now(uuid(&lib["id"]));
    wait("the file to be converted", async || done(&app).await == 2).await;
    assert_eq!(path_error(&app, &lib).await, None);
}

/// Choosing an output folder on a share that stopped answering doesn't
/// hold the save: it is refused within seconds, saying the folder isn't
/// responding, and nothing changes. Before, the save waited on the share
/// for as long as it hung.
#[tokio::test]
async fn choosing_a_folder_that_isnt_responding_answers_at_once() {
    let app = TestApp::new().await;
    let out = app.dir.path().join("hung-out");
    std::fs::create_dir_all(&out).unwrap();
    let _hung = hang::hang(&out);
    let started = Instant::now();
    let r = app
        .patch(
            "/api/settings",
            json!({ "output_mode": "folder", "output_folder": out.to_str().unwrap() }),
        )
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    assert_eq!(r.json["field"], "output_folder", "{}", r.json);
    let message = r.json["error"].as_str().unwrap_or_default();
    assert!(message.contains("isn't responding"), "{message}");
    assert_eq!(
        app.get("/api/settings").await.json["output_mode"],
        "replace"
    );
}

/// A work folder on a share with another drive put in its place: the
/// library whose job waits for it names the place (`changed_mount`), and
/// its "Use the drive that's there now" action takes it for the work
/// folder too, so the job goes on.
#[tokio::test]
async fn the_library_action_takes_another_drive_at_the_work_folder() {
    let app = TestApp::new().await;
    app.pause().await;
    let work = app.dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mounted = hang::mount(&work);
    let r = app
        .patch(
            "/api/settings",
            json!({ "temp_dir": work.to_str().unwrap() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.write("Movies/film.mp4", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let lib_id = lib["id"].as_str().unwrap().to_string();

    let _away = unmount(&work, mounted);
    let _other = hang::mount_other(&work, "cache pool");
    app.resume().await;
    wait("the library to wait for the work folder", async || {
        library(&app, &lib).await["changed_mount"] == work.to_str().unwrap()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(app.fake.started().is_empty(), "the job waits");
    let r = app.get("/api/settings/folders").await;
    let temp = r
        .json
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["setting"] == "temp_dir")
        .cloned();
    let temp = temp.unwrap_or_else(|| panic!("{}", r.json));
    assert_eq!(temp["changed_mount"], work.to_str().unwrap(), "{temp}");

    let r = app
        .post_empty(&format!("/api/libraries/{lib_id}/relearn-mounts"))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    wait("the file to be converted", async || done(&app).await == 1).await;
    assert_eq!(path_error(&app, &lib).await, None);
    let r = app.get("/api/settings/folders").await;
    assert!(
        r.json
            .as_array()
            .unwrap()
            .iter()
            .all(|f| f["problem"].is_null()),
        "{}",
        r.json
    );
}

/// The output folder is given as a link to a share's mount point: the share
/// is remembered through the link, so once it is unmounted the next job
/// waits ("isn't connected") instead of converting into the bare mount
/// point, and converts once the share is back. Before, the link wasn't
/// followed and the share was never remembered.
#[tokio::test]
async fn an_output_folder_given_as_a_link_to_a_share_is_on_that_share() {
    let app = TestApp::new().await;
    app.pause().await;
    let share = app.dir.path().join("share");
    std::fs::create_dir_all(&share).unwrap();
    let mounted = hang::mount(&share);
    let link = app.dir.path().join("out");
    std::os::unix::fs::symlink(&share, &link).unwrap();
    let r = app
        .patch(
            "/api/settings",
            json!({ "output_mode": "folder", "output_folder": link.to_str().unwrap() }),
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
    let known = crate::db::folder_mounts::get(app.state.db.pool(), link.to_str().unwrap())
        .await
        .unwrap();
    assert!(
        known
            .iter()
            .any(|m| m.mount == share.to_str().unwrap()
                && m.identity == Some(hang::share_at(&share))),
        "the share is remembered through the link: {known:?}"
    );

    let away = unmount(&share, mounted);
    app.write("Movies/later.mp4", h264());
    app.rescan(lib["id"].as_str().unwrap()).await;
    app.resume().await;
    wait("the library to wait for the share", async || {
        path_error(&app, &lib).await.is_some_and(|e| {
            e.contains("isn't connected") && e.contains(&format!("mounted at {}", share.display()))
        })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!app.fake.started().iter().any(|n| n == "later.mp4"));
    assert!(
        names(&share).is_empty(),
        "nothing written: {:?}",
        names(&share)
    );

    mend(&share, &away);
    let _mounted = hang::mount(&share);
    app.state.dispatcher.recheck_now(uuid(&lib["id"]));
    wait("the file to be converted", async || done(&app).await == 2).await;
    assert_eq!(names(&share), ["film.mkv", "later.mkv"]);
}

/// A share mounted inside the output folder (`out/Sub`, where the files of
/// the library's `Sub` folder go) is remembered with the output folder:
/// once it is unmounted, the next job waits ("isn't connected") instead of
/// converting into the bare mount point, and converts once it is back.
/// Before, only the mounts the output folder itself sits on were
/// remembered.
#[tokio::test]
async fn a_share_inside_the_output_folder_is_remembered_with_it() {
    let app = TestApp::new().await;
    app.pause().await;
    let out = app.dir.path().join("out");
    let sub = out.join("Sub");
    std::fs::create_dir_all(&sub).unwrap();
    let mounted = hang::mount(&sub);
    let r = app
        .patch(
            "/api/settings",
            json!({ "output_mode": "folder", "output_folder": out.to_str().unwrap() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.write("Movies/Sub/film.mp4", h264());
    let lib = app.add_library("Movies", json!({})).await;
    app.resume().await;
    wait("the first file to be converted", async || {
        done(&app).await == 1
    })
    .await;
    app.pause().await;
    assert_eq!(names(&sub), ["film.mkv"]);

    let away = unmount(&sub, mounted);
    app.write("Movies/Sub/later.mp4", h264());
    app.rescan(lib["id"].as_str().unwrap()).await;
    app.resume().await;
    wait("the library to wait for the share", async || {
        path_error(&app, &lib).await.is_some_and(|e| {
            e.contains("isn't connected") && e.contains(&format!("mounted at {}", sub.display()))
        })
    })
    .await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!app.fake.started().iter().any(|n| n == "later.mp4"));
    assert!(names(&sub).is_empty(), "nothing written: {:?}", names(&sub));

    mend(&sub, &away);
    let _mounted = hang::mount(&sub);
    app.state.dispatcher.recheck_now(uuid(&lib["id"]));
    wait("the file to be converted", async || done(&app).await == 2).await;
    assert_eq!(names(&sub), ["film.mkv", "later.mkv"]);
}

/// A library folder given as a link to a share's mount point is on that
/// share too: unmounted, the library is "not connected", even with a file
/// of its own in the bare mount point.
#[tokio::test]
async fn a_library_folder_given_as_a_link_to_a_share_is_on_that_share() {
    let app = TestApp::new().await;
    app.pause().await;
    let share = app.dir.path().join("library share");
    std::fs::create_dir_all(&share).unwrap();
    std::fs::write(share.join("film.mp4"), h264()).unwrap();
    let mounted = hang::mount(&share);
    std::os::unix::fs::symlink(&share, app.media.join("Movies")).unwrap();
    let lib = app.add_library("Movies", json!({})).await;
    assert_eq!(path_error(&app, &lib).await, None);
    assert_eq!(app.get("/api/files").await.json["total"], 1);

    let _away = unmount(&share, mounted);
    std::fs::write(share.join("README.txt"), "the disk below").unwrap();
    let reason = path_error(&app, &lib).await.unwrap_or_default();
    assert!(
        reason.contains(&format!(
            "The drive or share mounted at {} isn't connected",
            share.display()
        )),
        "{reason}"
    );
    app.rescan(lib["id"].as_str().unwrap()).await;
    assert_eq!(app.get("/api/files").await.json["total"], 1, "still listed");
    app.resume().await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(app.fake.started().is_empty());
    assert_eq!(names(&share), ["README.txt"], "nothing written there");
}

/// A library's share stops answering, is unmounted lazily (`umount -l`,
/// its checks stay stuck on the old mount) and mounted again at the same
/// place: the library answers again and its file converts. Before, every
/// new look at the library folder joined the look stuck on the old mount,
/// so it stayed "isn't responding" for as long as that hung.
#[tokio::test]
async fn a_hung_share_mounted_again_at_its_place_comes_back() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Nas/film.mp4", h264());
    let nas = app.media.join("Nas");
    let first = hang::mount(&nas);
    let lib = app.add_library("Nas", json!({})).await;
    assert_eq!(path_error(&app, &lib).await, None);
    assert_eq!(app.get("/api/files").await.json["total"], 1);

    let hung = hang::hang_mount(&first);
    wait("the library to stop answering", async || {
        path_error(&app, &lib)
            .await
            .is_some_and(|e| e.contains("isn't responding"))
    })
    .await;
    app.resume().await;
    wait("its job to wait", async || {
        app.state
            .dispatcher
            .offline_reason(uuid(&lib["id"]))
            .is_some()
    })
    .await;

    // Unmounted lazily (the stuck looks hang on), and mounted again.
    drop(first);
    let _again = hang::mount(&nas);
    wait("the library to answer again", async || {
        path_error(&app, &lib).await.is_none()
    })
    .await;
    app.state.dispatcher.recheck_now(uuid(&lib["id"]));
    wait("the file to be converted", async || done(&app).await == 1).await;
    assert_eq!(path_error(&app, &lib).await, None);
    drop(hung);
}
