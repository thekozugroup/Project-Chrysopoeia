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
}

/// The library `Movies` with one file, converted with the worker's own
/// `finalize` held at the step that leaves the disk as the share would:
/// the new file under its name with the original's backup next to it
/// (`placed`), or the original moved aside and the new file not in place.
/// The server stops meanwhile (Stop asks the step to undo itself, it can't,
/// the server ends without it), with the queue paused when `pause` is set.
async fn stopped_while_placing(kind: Kind, placed: bool, pause: bool) -> (TempDir, Stopped) {
    let app = TestApp::new().await;
    app.pause().await;
    let out = app.dir.path().join("out");
    if kind == Kind::Folder {
        std::fs::create_dir_all(&out).unwrap();
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
