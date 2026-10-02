//! The server stops while a new file is being put in place on a share that
//! stopped answering, and the share finishes that rename (or doesn't) after
//! the server is gone: whatever was recorded at the stop, every restart
//! ends with one file in the library and a record that says what happened.
//! The new file in place → the job is done (with a note) and the backup of
//! the original is gone; not in place → the original is back and the file
//! is converted again. Nothing touches the job's backup before that is
//! known: not start-up's search for leftovers, not a scan.
//!
//! The fake converter puts its new file in place with the worker's own
//! `finalize`, held at a chosen step with `finalize::hold` (a rename that
//! waits for the share); `fs_guard::hang` stands in for the share at the
//! restart.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use chrysopoeia_worker::finalize::hold::{self, Step};
use serde_json::{Value, json};
use tempfile::TempDir;
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

async fn job_of(app: &TestApp, file_name: &str) -> String {
    let jobs = app.get("/api/jobs?limit=500").await;
    jobs.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["file_name"] == file_name)
        .unwrap_or_else(|| panic!("no job for {file_name}: {}", jobs.json))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn job(app: &TestApp, id: &str) -> Value {
    app.get(&format!("/api/jobs/{id}")).await.json
}

fn uuid(id: &str) -> Uuid {
    Uuid::parse_str(id).unwrap()
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

/// How the conversion was putting its new file in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `film.mp4` replaced by `film.mkv`.
    NewName,
    /// `film.mkv` replaced by `film.mkv`.
    SameName,
    /// `film.mp4` converted into the output folder as `film.mkv`.
    Folder,
}

/// What a stopped server left behind.
struct Stopped {
    job: String,
    /// The original file.
    original: PathBuf,
    /// Where the new file goes.
    target: PathBuf,
    /// The step the new file's placing is held at (as long as this lives,
    /// the rename stays where the share left it).
    held: hold::Held,
    /// The share's mount, when the share is one of its own (see
    /// [`stopped_on_share`]); dropping it unmounts the share.
    mounted: Option<hang::Marked>,
}

/// The library `Movies` with one file, converted with the worker's own
/// `finalize` held at the step that leaves the disk as the share would:
/// the new file under its name with the original's backup next to it
/// (`placed`), or the original moved aside and the new file not in place.
/// The server stops meanwhile (Stop asks the step to undo itself, it can't,
/// the server ends without it), with the queue paused when `pause` is set.
async fn stopped_while_placing(kind: Kind, placed: bool, pause: bool) -> (TempDir, Stopped) {
    stop_while_placing(kind, placed, pause, false).await
}

/// [`stopped_while_placing`], with the share (the library folder for a
/// replacement, the output folder in folder mode) a mount of its own,
/// mounted while the server ran: [`Stopped::mounted`].
async fn stopped_on_share(kind: Kind, placed: bool, pause: bool) -> (TempDir, Stopped) {
    stop_while_placing(kind, placed, pause, true).await
}

async fn stop_while_placing(
    kind: Kind,
    placed: bool,
    pause: bool,
    on_share: bool,
) -> (TempDir, Stopped) {
    let app = TestApp::new().await;
    app.pause().await;
    let out = app.dir.path().join("out");
    let mut mounted = None;
    if kind == Kind::Folder {
        std::fs::create_dir_all(&out).unwrap();
        if on_share {
            mounted = Some(hang::mount(&out));
        }
        let r = app
            .patch(
                "/api/settings",
                json!({ "output_mode": "folder", "output_folder": out.to_str().unwrap() }),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }
    let name = if kind == Kind::SameName {
        "film.mkv"
    } else {
        "film.mp4"
    };
    let original = std::fs::canonicalize(app.write(&format!("Movies/{name}"), h264())).unwrap();
    if on_share && kind != Kind::Folder {
        mounted = Some(hang::mount(original.parent().unwrap()));
    }
    app.fake.set_behavior(name, Behavior::RealPlacing);
    app.add_library("Movies", json!({})).await;
    let job_id = job_of(&app, name).await;
    let target = if kind == Kind::Folder {
        std::fs::canonicalize(&out).unwrap().join("film.mkv")
    } else {
        original.with_file_name("film.mkv")
    };
    let step = match (placed, kind) {
        (true, _) => Step::Committed,
        // Folder mode never moves the original aside.
        (false, Kind::Folder) => Step::Start,
        (false, _) => Step::MovedAside,
    };
    let held = hold::hold(uuid(&job_id), step);
    app.resume().await;
    wait("the new file to be on its way", async || held.reached()).await;
    if pause {
        app.pause().await;
    }
    let dir = app.stop().await;
    let stopped = Stopped {
        job: job_id,
        original,
        target,
        held,
        mounted,
    };
    (dir, stopped)
}

async fn restart(dir: TempDir) -> (TestApp, Arc<FakeToolkit>) {
    let fake = Arc::new(FakeToolkit::default());
    let app = TestApp::start(TestOptions {
        dir: Some(dir),
        fake: Arc::clone(&fake),
        ..TestOptions::default()
    })
    .await;
    (app, fake)
}

/// The job is done with the note of a conversion finished after a stop,
/// the file was not converted again, and only the new file is left (the
/// original, in folder mode, where it was).
async fn assert_finished(app: &TestApp, fake: &FakeToolkit, s: &Stopped, kind: Kind) {
    let j = job(app, &s.job).await;
    assert_eq!(j["state"], "done", "{j}");
    assert!(j["notes"].to_string().contains("already complete"), "{j}");
    assert!(fake.started().is_empty(), "not converted again");
    let library = s.original.parent().unwrap();
    match kind {
        Kind::NewName | Kind::SameName => {
            assert_eq!(names(library), ["film.mkv"], "one file, no backup");
            let new = std::fs::read_to_string(&s.target).unwrap();
            assert!(!new.contains("video=h264"), "the new file: {new}");
        }
        Kind::Folder => {
            assert_eq!(names(library), ["film.mp4"], "the original stays");
            assert_eq!(names(s.target.parent().unwrap()), ["film.mkv"]);
        }
    }
    let file_id = j["file_id"].as_str().unwrap();
    let f = app.get(&format!("/api/files/{file_id}")).await.json;
    assert_eq!(f["file"]["status"], "done", "{f}");
    assert!(f["file"]["saved_bytes"].as_i64().unwrap_or(0) > 0, "{f}");
}

/// Stopped while the share held the rename that gives the new file its
/// name; the share made it after the server was gone: the next start
/// records the conversion as done, with its savings, and removes the
/// backup, whatever the stop recorded (the job was back in the queue).
/// Before, the start-up search put the original back next to the new file
/// and the job failed on the name ("Skipped: Already …" for the same name).
#[tokio::test]
async fn a_new_file_the_share_put_in_place_after_a_stop_is_recorded() {
    for kind in [Kind::NewName, Kind::SameName, Kind::Folder] {
        let (dir, s) = stopped_while_placing(kind, true, false).await;
        let (app, fake) = restart(dir).await;
        wait("the job to be recorded as done", async || {
            job(&app, &s.job).await["state"] == "done"
        })
        .await;
        app.wait_queue_idle().await;
        assert_finished(&app, &fake, &s, kind).await;
        drop(s.held);
    }
}

/// Stopped with the original moved aside and the new file not in place (the
/// share never made that rename): the next start puts the original back,
/// and the file is converted again; one file is left.
#[tokio::test]
async fn an_original_left_aside_by_a_stop_is_put_back_and_converted_again() {
    for kind in [Kind::NewName, Kind::SameName, Kind::Folder] {
        let (dir, s) = stopped_while_placing(kind, false, false).await;
        let library = s.original.parent().unwrap().to_path_buf();
        let (app, fake) = restart(dir).await;
        wait("the file to be converted again", async || {
            job(&app, &s.job).await["state"] == "done"
        })
        .await;
        app.wait_queue_idle().await;
        let j = job(&app, &s.job).await;
        assert!(!j["notes"].to_string().contains("already complete"), "{j}");
        // Converted from the original, put back where it was.
        let specs = fake.run_specs.lock().unwrap().clone();
        assert_eq!(specs.len(), 1, "{kind:?}: converted once more");
        assert_eq!(specs[0].input, s.original, "{kind:?}");
        match kind {
            Kind::NewName | Kind::SameName => assert_eq!(names(&library), ["film.mkv"]),
            Kind::Folder => {
                assert_eq!(names(&library), ["film.mp4"]);
                assert_eq!(names(s.target.parent().unwrap()), ["film.mkv"]);
            }
        }
        drop(s.held);
    }
}

/// The share still hangs at the restart, so start-up can't tell; a scan
/// that finds the job's backup once the share answers leaves it alone
/// (before, it put the original back next to the new file), and the job,
/// when it runs, finds its new file in place and is recorded as done.
#[tokio::test]
async fn nothing_touches_the_backup_before_the_job_settles_it() {
    let (dir, s) = stopped_while_placing(Kind::NewName, true, true).await;
    let library = s.original.parent().unwrap().to_path_buf();
    let hung = hang::hang(&library);
    let (app, fake) = restart(dir).await;
    drop(hung);
    let libs = app.get("/api/libraries").await.json;
    let lib_id = libs[0]["id"].as_str().unwrap().to_string();
    app.rescan(&lib_id).await;
    // The scan found the backup and left it: the original is not back, and
    // its row is still listed.
    let left = names(&library);
    assert!(left.contains(&"film.mkv".to_string()), "{left:?}");
    assert!(!left.contains(&"film.mp4".to_string()), "{left:?}");
    assert!(
        left.iter().any(|n| n.ends_with(".bak")),
        "the backup is left alone: {left:?}"
    );
    assert_eq!(job(&app, &s.job).await["state"], "queued");

    app.resume().await;
    wait("the job to be recorded as done", async || {
        job(&app, &s.job).await["state"] == "done"
    })
    .await;
    app.wait_queue_idle().await;
    assert_finished(&app, &fake, &s, Kind::NewName).await;
    drop(s.held);
}

/// The job was cancelled while its new file was being put in place (too
/// late to undo), and the server stopped before the share answered; at the
/// restart the share still hangs. The cancelled job never runs again, so
/// it is looked at again by itself once the share answers: done.
#[tokio::test]
async fn a_cancelled_job_whose_new_file_got_in_place_is_recorded_once_the_share_answers() {
    let app = TestApp::new().await;
    app.pause().await;
    let original = std::fs::canonicalize(app.write("Movies/film.mp4", h264())).unwrap();
    app.fake.set_behavior("film.mp4", Behavior::RealPlacing);
    app.add_library("Movies", json!({})).await;
    let job_id = job_of(&app, "film.mp4").await;
    let held = hold::hold(uuid(&job_id), Step::Committed);
    app.resume().await;
    wait("the new file to be in place", async || held.reached()).await;
    let r = app.post_empty(&format!("/api/jobs/{job_id}/cancel")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["state"], "cancelled", "{}", r.json);
    let dir = app.stop().await;

    let library = original.parent().unwrap().to_path_buf();
    let hung = hang::hang(&library);
    let (app, fake) = restart(dir).await;
    // Start-up couldn't look.
    assert_eq!(job(&app, &job_id).await["state"], "cancelled");
    drop(hung);
    wait("the job to be recorded as done", async || {
        job(&app, &job_id).await["state"] == "done"
    })
    .await;
    let s = Stopped {
        job: job_id,
        original,
        target: library.join("film.mkv"),
        held,
        mounted: None,
    };
    assert_finished(&app, &fake, &s, Kind::NewName).await;
    drop(s.held);
}

/// While a new file is still being put in place after its job ended (the
/// job was cancelled too late to undo it), a scan leaves the job's backup
/// alone and keeps the file listed although its original is moved aside;
/// once the step ends, the job is recorded as done and one file is left.
#[tokio::test]
async fn a_scan_leaves_a_new_file_that_is_still_moving_in_alone() {
    let app = TestApp::new().await;
    app.pause().await;
    let original = std::fs::canonicalize(app.write("Movies/film.mp4", h264())).unwrap();
    app.fake.set_behavior("film.mp4", Behavior::RealPlacing);
    let lib = app.add_library("Movies", json!({})).await;
    let lib_id = lib["id"].as_str().unwrap().to_string();
    let job_id = job_of(&app, "film.mp4").await;
    let held = hold::hold(uuid(&job_id), Step::Committed);
    app.resume().await;
    wait("the new file to be in place", async || held.reached()).await;
    let r = app.post_empty(&format!("/api/jobs/{job_id}/cancel")).await;
    assert_eq!(r.json["state"], "cancelled", "{}", r.json);
    assert!(app.state.dispatcher.is_placing(uuid(&job_id)));

    app.rescan(&lib_id).await;
    let library = original.parent().unwrap();
    assert!(
        names(library).iter().any(|n| n.ends_with(".bak")),
        "the backup is left alone: {:?}",
        names(library)
    );
    let file_id = job(&app, &job_id).await["file_id"]
        .as_str()
        .unwrap()
        .to_string();
    let f = app.get(&format!("/api/files/{file_id}")).await;
    assert_eq!(f.status, StatusCode::OK, "still listed: {}", f.text);

    drop(held);
    wait("the job to be recorded as done", async || {
        job(&app, &job_id).await["state"] == "done"
    })
    .await;
    let j = job(&app, &job_id).await;
    assert!(j["notes"].to_string().contains("too far to undo"), "{j}");
    wait("one file to be left", async || {
        names(library) == ["film.mkv"]
    })
    .await;
    let f = app.get(&format!("/api/files/{file_id}")).await.json;
    assert_eq!(f["file"]["status"], "done", "{f}");
    assert_eq!(f["file"]["file_name"], "film.mkv", "{f}");
}

/// The share answered just as the server stopped: the step put the new
/// file in place and removed the backup, but the result couldn't be
/// recorded any more. The new file, with the size the step was putting in
/// place, under its name while the original's is free, still tells: the
/// job is recorded as done instead of converting a file that is gone. A
/// file of another size there is not taken for it.
#[tokio::test]
async fn a_new_file_put_in_place_as_the_server_stopped_is_recorded_without_its_backup() {
    for matches in [true, false] {
        let app = TestApp::new().await;
        app.pause().await;
        let original = std::fs::canonicalize(app.write("Movies/film.mp4", h264())).unwrap();
        app.add_library("Movies", json!({})).await;
        let job_id = job_of(&app, "film.mp4").await;
        let target = original.with_file_name("film.mkv");
        let new = "video=av1\naudio=aac\nwidth=1920\nheight=1080\n";
        sqlx::query("UPDATE jobs SET final_path = ? WHERE id = ?")
            .bind(target.to_str().unwrap())
            .bind(&job_id)
            .execute(app.state.db.pool())
            .await
            .unwrap();
        let size = new.len() as u64 + u64::from(!matches);
        crate::db::jobs::mark_placing(app.state.db.pool(), uuid(&job_id), Some(size))
            .await
            .unwrap();
        std::fs::remove_file(&original).unwrap();
        std::fs::write(&target, new).unwrap();

        app.resume().await;
        let library = original.parent().unwrap();
        if matches {
            wait("the job to be recorded as done", async || {
                job(&app, &job_id).await["state"] == "done"
            })
            .await;
            let j = job(&app, &job_id).await;
            assert!(j["notes"].to_string().contains("already complete"), "{j}");
            assert!(app.fake.started().is_empty(), "not converted again");
            assert_eq!(names(library), ["film.mkv"]);
        } else {
            // Not taken for its new file: the job runs and finds its file
            // gone, as before.
            wait("the job to end", async || {
                !matches!(
                    job(&app, &job_id).await["state"].as_str(),
                    Some("queued" | "running")
                )
            })
            .await;
            let j = job(&app, &job_id).await;
            assert!(!j["notes"].to_string().contains("already complete"), "{j}");
        }
    }
}

/// How a share answers at a restart when it isn't there as it should be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Broken {
    /// Every look in it fails with an error (here a file stands where the
    /// folder was, so looks inside fail with "not a folder", as a
    /// soft-mounted share answers "read or write error" and a FUSE share
    /// whose server stopped answers "not connected").
    Errors,
    /// It isn't mounted: its folder is there, empty.
    Unmounted,
}

/// Make `folder` answer as `how` says; it comes back with [`mend`].
fn break_share(folder: &Path, how: Broken) -> PathBuf {
    let away = folder.with_extension("away");
    std::fs::rename(folder, &away).unwrap();
    match how {
        Broken::Errors => std::fs::write(folder, "not a folder").unwrap(),
        Broken::Unmounted => std::fs::create_dir(folder).unwrap(),
    }
    away
}

fn mend(folder: &Path, away: &Path) {
    if folder.is_dir() {
        std::fs::remove_dir(folder).unwrap();
    } else {
        std::fs::remove_file(folder).unwrap();
    }
    std::fs::rename(away, folder).unwrap();
}

/// The job waits: back in the queue, or looking again for a moment (a scan
/// that finds the library's files lets it run again, and it goes straight
/// back), never done, failed or cancelled.
async fn assert_waiting(app: &TestApp, id: &str, kind: Kind) {
    let j = job(app, id).await;
    assert!(
        matches!(j["state"].as_str(), Some("queued" | "running")),
        "{kind:?}: {j}"
    );
}

async fn placing(app: &TestApp, job: &str) -> i64 {
    sqlx::query_scalar("SELECT placing FROM jobs WHERE id = ?")
        .bind(job)
        .fetch_one(app.state.db.pool())
        .await
        .unwrap()
}

/// Where the share is: the library folder for a replacement, the output
/// folder in folder mode.
fn share_of(s: &Stopped, kind: Kind) -> PathBuf {
    match kind {
        Kind::NewName | Kind::SameName => s.original.parent().unwrap().to_path_buf(),
        Kind::Folder => s.target.parent().unwrap().to_path_buf(),
    }
}

/// The share put the new file in place after the stop, but at the restart
/// it answers every look with an error (or isn't mounted): that tells
/// nothing, so nothing is settled and the job keeps its mark. Once the
/// share works again, a scan leaves the job's backup alone, and the job,
/// when it runs, is recorded as done with its savings. Before, start-up
/// took the errors for "not in place" and cleared the mark; the scan then
/// put the original back next to the new file, and the job failed on the
/// name ("Skipped: Already …" for the same name; in folder mode it failed
/// at once on its own new file).
#[tokio::test]
async fn a_share_answering_with_errors_at_the_restart_settles_nothing() {
    let cases = [
        (Kind::NewName, Broken::Errors),
        (Kind::SameName, Broken::Errors),
        (Kind::Folder, Broken::Errors),
        (Kind::NewName, Broken::Unmounted),
        (Kind::SameName, Broken::Unmounted),
    ];
    for (kind, how) in cases {
        let (dir, s) = stopped_while_placing(kind, true, true).await;
        let share = share_of(&s, kind);
        let before = names(&share);
        let away = break_share(&share, how);
        let (app, fake) = restart(dir).await;
        assert_eq!(
            placing(&app, &s.job).await,
            1,
            "{kind:?} {how:?}: still marked"
        );
        assert_eq!(
            job(&app, &s.job).await["state"],
            "queued",
            "{kind:?} {how:?}"
        );
        assert_eq!(names(&away), before, "{kind:?} {how:?}: nothing touched");

        mend(&share, &away);
        let libs = app.get("/api/libraries").await.json;
        let lib_id = libs[0]["id"].as_str().unwrap().to_string();
        app.rescan(&lib_id).await;
        assert_eq!(
            names(&share),
            before,
            "{kind:?} {how:?}: the scan left the job's files alone"
        );
        assert_eq!(placing(&app, &s.job).await, 1, "{kind:?} {how:?}");

        app.resume().await;
        wait("the job to be recorded as done", async || {
            job(&app, &s.job).await["state"] == "done"
        })
        .await;
        app.wait_queue_idle().await;
        assert_finished(&app, &fake, &s, kind).await;
        drop(s.held);
    }
}

/// The share still answers with errors when the job runs again: the job
/// waits with its library shown as unreachable (not failed, nothing
/// touched), and finishes as done once the share works.
#[tokio::test]
async fn a_job_waits_while_its_share_answers_with_errors() {
    for kind in [Kind::NewName, Kind::Folder] {
        let (dir, s) = stopped_while_placing(kind, true, false).await;
        let share = share_of(&s, kind);
        let before = names(&share);
        let away = break_share(&share, Broken::Errors);
        let (app, fake) = restart(dir).await;
        let libs = app.get("/api/libraries").await.json;
        let lib_id = libs[0]["id"].as_str().unwrap().to_string();
        // The job runs as soon as the server is ready, finds it can't tell,
        // and goes back to the queue with its library waiting (a scan that
        // finds the library's files may let it run again meanwhile; it
        // waits again, the same way).
        wait("the job to wait for its share", async || {
            let reason = app.get(&format!("/api/libraries/{lib_id}")).await.json["path_error"]
                .as_str()
                .map(str::to_string);
            // The library folder itself is the share for a replacement (its
            // own problem is shown); in folder mode the output folder is.
            let says_why = reason.is_some_and(|r| {
                kind != Kind::Folder
                    || (r.contains("can't read the folder") && r.contains("check that it's"))
            });
            says_why
                && app.state.dispatcher.offline_reason(uuid(&lib_id)).is_some()
                && job(&app, &s.job).await["state"] == "queued"
        })
        .await;
        assert!(fake.started().is_empty(), "{kind:?}: not converted again");
        assert_eq!(placing(&app, &s.job).await, 1, "{kind:?}");
        assert_eq!(names(&away), before, "{kind:?}: nothing touched");

        mend(&share, &away);
        app.state.dispatcher.recheck_now(uuid(&lib_id));
        wait("the job to be recorded as done", async || {
            job(&app, &s.job).await["state"] == "done"
        })
        .await;
        app.wait_queue_idle().await;
        assert_finished(&app, &fake, &s, kind).await;
        drop(s.held);
    }
}

/// The library's id (the only library).
async fn only_library(app: &TestApp) -> String {
    let libs = app.get("/api/libraries").await.json;
    libs[0]["id"].as_str().unwrap().to_string()
}

/// The share put the new file in place after the stop, and was then
/// unmounted cleanly while the server was down: its mount point is an
/// ordinary folder at the restart (empty in folder mode, where the output
/// folder is the share; with a file of its own where the library folder
/// is the share). Neither is taken for the share: the job keeps its mark,
/// waits with "isn't connected", nothing is written into the folder and a
/// scan restores nothing. Once the share is mounted again, the job is
/// recorded as done. Before, start-up cleared the mark ("wasn't in
/// place"): in folder mode the file was converted again into the bare
/// mount point (and the job failed when the share came back during that
/// run), and for a replacement the scan put the original back next to the
/// new file once the share was back.
#[tokio::test]
async fn an_unmounted_share_is_never_taken_for_its_mount_point() {
    for kind in [Kind::Folder, Kind::NewName] {
        let (dir, mut s) = stopped_on_share(kind, true, true).await;
        let share = share_of(&s, kind);
        let before = names(&share);
        // Unmounted: the mount point is left, empty, or (the library
        // folder) with a file of its own.
        let away = break_share(&share, Broken::Unmounted);
        if kind == Kind::NewName {
            std::fs::write(share.join("README.txt"), "the disk below").unwrap();
        }
        let bare = names(&share);
        drop(s.mounted.take());

        let (app, fake) = restart(dir).await;
        assert_eq!(placing(&app, &s.job).await, 1, "{kind:?}: still marked");
        assert_eq!(job(&app, &s.job).await["state"], "queued", "{kind:?}");
        let lib_id = only_library(&app).await;
        app.rescan(&lib_id).await;
        app.resume().await;
        wait("the library to wait for its share", async || {
            app.get(&format!("/api/libraries/{lib_id}")).await.json["path_error"]
                .as_str()
                .is_some_and(|e| e.contains("isn't connected"))
        })
        .await;
        let lib = app.get(&format!("/api/libraries/{lib_id}")).await.json;
        let reason = lib["path_error"].as_str().unwrap_or_default().to_string();
        assert!(
            reason.contains(&format!("mounted at {}", share.display())),
            "{kind:?}: {reason}"
        );
        // A moment for anything that would still go wrong.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_waiting(&app, &s.job, kind).await;
        assert_eq!(placing(&app, &s.job).await, 1, "{kind:?}");
        assert!(fake.started().is_empty(), "{kind:?}: not converted again");
        assert_eq!(names(&share), bare, "{kind:?}: nothing written there");
        assert_eq!(names(&away), before, "{kind:?}: nothing touched");

        // Mounted again (another mount, at the same place).
        if kind == Kind::NewName {
            std::fs::remove_file(share.join("README.txt")).unwrap();
        }
        mend(&share, &away);
        let _mounted = hang::mount(&share);
        app.rescan(&lib_id).await;
        // (The job may settle itself as soon as the library is back,
        // removing its backup.)
        let left = names(&share);
        assert!(
            left.contains(&"film.mkv".to_string()) && !left.contains(&"film.mp4".to_string()),
            "{kind:?}: the scan restored nothing: {left:?}"
        );
        app.state.dispatcher.recheck_now(uuid(&lib_id));
        wait("the job to be recorded as done", async || {
            job(&app, &s.job).await["state"] == "done"
        })
        .await;
        app.wait_queue_idle().await;
        assert_finished(&app, &fake, &s, kind).await;
        drop(s.held);
    }
}

/// The share put the new file in place after the stop, and at the restart
/// another filesystem is mounted where it was: a tmpfs, or the bare folder
/// bind-mounted onto itself (what a Docker bind mount shows when the
/// container started before the host mounted the share; on Unraid, a
/// remote share mounted by Unassigned Devices after the array started).
/// It isn't taken for the share: the job keeps its mark, waits with "A
/// different drive is mounted at …", nothing is written there and a scan
/// restores nothing. Once the usual share is mounted again, the job is
/// recorded as done. Before, any filesystem at that place counted as the
/// share: start-up settled the job as "not in place"; in folder mode the
/// file was converted again into the other filesystem (and the job failed
/// once the share was back), and for a replacement over a folder with
/// files of its own the scan later restored the original next to the new
/// file.
#[tokio::test]
async fn another_filesystem_at_the_share_is_never_taken_for_it() {
    for kind in [Kind::Folder, Kind::NewName] {
        let (dir, mut s) = stopped_on_share(kind, true, true).await;
        let share = share_of(&s, kind);
        let before = names(&share);
        // The share is gone, and the folder below it (with a file of its
        // own for the library) is mounted in its place.
        let away = break_share(&share, Broken::Unmounted);
        if kind == Kind::NewName {
            std::fs::write(share.join("README.txt"), "the disk below").unwrap();
        }
        let bare = names(&share);
        drop(s.mounted.take());
        let other = hang::mount_other(&share, "the bare folder");

        let (app, fake) = restart(dir).await;
        assert_eq!(placing(&app, &s.job).await, 1, "{kind:?}: still marked");
        assert_eq!(job(&app, &s.job).await["state"], "queued", "{kind:?}");
        let lib_id = only_library(&app).await;
        app.rescan(&lib_id).await;
        app.resume().await;
        let different = format!(
            "A different drive is mounted at {} than before",
            share.display()
        );
        wait("the library to wait for its share", async || {
            app.get(&format!("/api/libraries/{lib_id}")).await.json["path_error"]
                .as_str()
                .is_some_and(|e| e.contains(&different))
        })
        .await;
        let lib = app.get(&format!("/api/libraries/{lib_id}")).await.json;
        assert_eq!(
            lib["changed_mount"],
            share.to_str().unwrap(),
            "{kind:?}: {lib}"
        );
        // A moment for anything that would still go wrong.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_waiting(&app, &s.job, kind).await;
        assert_eq!(placing(&app, &s.job).await, 1, "{kind:?}");
        assert!(fake.started().is_empty(), "{kind:?}: not converted again");
        assert_eq!(names(&share), bare, "{kind:?}: nothing written there");
        assert_eq!(names(&away), before, "{kind:?}: nothing touched");

        // The usual share mounted again (over the other one, unmounted).
        drop(other);
        if kind == Kind::NewName {
            std::fs::remove_file(share.join("README.txt")).unwrap();
        }
        mend(&share, &away);
        let _mounted = hang::mount(&share);
        app.rescan(&lib_id).await;
        let left = names(&share);
        assert!(
            left.contains(&"film.mkv".to_string()) && !left.contains(&"film.mp4".to_string()),
            "{kind:?}: the scan restored nothing: {left:?}"
        );
        app.state.dispatcher.recheck_now(uuid(&lib_id));
        wait("the job to be recorded as done", async || {
            job(&app, &s.job).await["state"] == "done"
        })
        .await;
        app.wait_queue_idle().await;
        assert_finished(&app, &fake, &s, kind).await;
        drop(s.held);
    }
}

/// Folder mode, with a share mounted inside the output folder, where the
/// new file goes (`out/Sub`): the share put the new file in place after the
/// stop, and was unmounted cleanly while the server was down (or another
/// filesystem was mounted in its place). The mount the new file went into
/// was noted when the job started (and the worker was told to check it):
/// at the restart the job keeps its mark and waits ("isn't connected", or
/// "A different drive …"), nothing is written into that folder, and once
/// the share is mounted again the job is recorded as done. Before, only the
/// output folder's own mounts were remembered: start-up settled the job as
/// "not in place" and the file was converted again into the bare folder.
#[tokio::test]
async fn a_share_inside_the_output_folder_is_never_taken_for_its_mount_point() {
    for other_there in [false, true] {
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
        let original = std::fs::canonicalize(app.write("Movies/Sub/film.mp4", h264())).unwrap();
        app.fake.set_behavior("film.mp4", Behavior::RealPlacing);
        app.add_library("Movies", json!({})).await;
        let job_id = job_of(&app, "film.mp4").await;
        let held = hold::hold(uuid(&job_id), Step::Committed);
        app.resume().await;
        wait("the new file to be on its way", async || held.reached()).await;
        let specs = app.fake.run_specs.lock().unwrap().clone();
        assert!(
            specs[0]
                .mounts
                .iter()
                .any(|m| m.point == sub && m.identity == Some(hang::share_at(&sub))),
            "the worker checks the share the new file goes into: {:?}",
            specs[0].mounts
        );
        app.pause().await;
        let dir = app.stop().await;
        let mut s = Stopped {
            job: job_id,
            original,
            target: std::fs::canonicalize(&sub).unwrap().join("film.mkv"),
            held,
            mounted: Some(mounted),
        };

        let before = names(&sub);
        let away = break_share(&sub, Broken::Unmounted);
        drop(s.mounted.take());
        let other = other_there.then(|| hang::mount_other(&sub, "tmpfs"));
        let (app, fake) = restart(dir).await;
        assert_eq!(placing(&app, &s.job).await, 1, "still marked");
        assert_eq!(job(&app, &s.job).await["state"], "queued");
        let lib_id = only_library(&app).await;
        app.rescan(&lib_id).await;
        app.resume().await;
        let expected = if other_there {
            format!(
                "A different drive is mounted at {} than before",
                sub.display()
            )
        } else {
            format!(
                "The drive or share mounted at {} isn't connected",
                sub.display()
            )
        };
        wait("the library to wait for the share", async || {
            app.get(&format!("/api/libraries/{lib_id}")).await.json["path_error"]
                .as_str()
                .is_some_and(|e| e.contains(&expected))
        })
        .await;
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_waiting(&app, &s.job, Kind::Folder).await;
        assert_eq!(placing(&app, &s.job).await, 1);
        assert!(fake.started().is_empty(), "not converted again");
        assert!(names(&sub).is_empty(), "nothing written: {:?}", names(&sub));
        assert_eq!(names(&away), before, "nothing touched");

        drop(other);
        mend(&sub, &away);
        let _mounted = hang::mount(&sub);
        app.state.dispatcher.recheck_now(uuid(&lib_id));
        wait("the job to be recorded as done", async || {
            job(&app, &s.job).await["state"] == "done"
        })
        .await;
        app.wait_queue_idle().await;
        assert_finished(&app, &fake, &s, Kind::Folder).await;
        drop(s.held);
    }
}
