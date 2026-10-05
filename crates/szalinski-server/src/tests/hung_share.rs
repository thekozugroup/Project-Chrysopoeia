//! A share that stops answering at any point of a job: the job never holds
//! its slot for good (other libraries' queues move), Cancel and Stop free
//! it within seconds, it goes back to the queue with its library offline
//! instead of failing or being taken for deleted, a cancel wins over going
//! back to the queue, and a new file still being put in place is followed
//! to its end instead of being abandoned.
//!
//! `fs_guard::hang` stands in for the share: every bounded check of a path
//! under a hung one blocks until it is lifted.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

use super::support::*;
use crate::services::dispatcher::CancelIntent;
use crate::services::fs_guard::hang;

/// Far more than anything here should take, even on a loaded machine.
const PATIENCE: Duration = Duration::from_secs(120);

/// Two libraries, `Share` (its file first in the queue) and `Healthy`, one
/// job at a time, the queue paused. Returns the app, the share's file (as
/// the database knows it), its job id and the share's library id.
async fn two_libraries(stuck: Behavior) -> (TestApp, PathBuf, String, String) {
    let app = TestApp::new().await;
    app.pause().await;
    app.patch("/api/settings", json!({ "max_jobs": 1 })).await;
    let file = app.write("Share/stuck.mkv", h264());
    app.write("Healthy/fine.mkv", h264());
    app.fake.set_behavior("stuck.mkv", stuck);
    let share = app.add_library("Share", json!({})).await;
    let file = std::fs::canonicalize(&file).unwrap();
    let job = job_of(&app, "stuck.mkv").await;
    app.post(
        &format!("/api/jobs/{job}/priority"),
        json!({ "priority": 10 }),
    )
    .await;
    app.add_library("Healthy", json!({})).await;
    let share = share["id"].as_str().unwrap().to_string();
    (app, file, job, share)
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

async fn wait(what: &str, f: impl AsyncFn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !f().await {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait until the share's job is back in the queue with the share shown
/// offline, its slot free and the healthy library's file converted.
async fn requeued_offline(app: &TestApp, job_id: &str, share: &str) {
    wait("the job to go back to the queue", async || {
        job(app, job_id).await["state"] == "queued"
    })
    .await;
    wait("the share to be shown offline", async || {
        app.state
            .dispatcher
            .offline_reason(uuid(share))
            .is_some_and(|r| r.contains("isn't responding"))
    })
    .await;
    wait("the healthy library's file to be converted", async || {
        app.fake.started().iter().any(|n| n == "fine.mkv")
            && app.get("/api/files?status=done").await.json["total"] == 1
    })
    .await;
    wait("the slot to be free", async || {
        app.state.dispatcher.running_count() == 0
    })
    .await;
    // Not failed, and not taken for a deleted file.
    let failed = app.get("/api/files?status=failed").await;
    assert_eq!(failed.json["total"], 0, "{}", failed.json);
    let lib = app.get(&format!("/api/libraries/{share}")).await.json;
    assert!(
        lib["path_error"]
            .as_str()
            .is_some_and(|e| e.contains("isn't responding")),
        "{lib}"
    );
}

/// The converter fails while the share hangs (the job's last look at its
/// file used to wait for the share for good, holding the only slot, deaf
/// to Cancel): the job goes back to the queue with its library offline,
/// not failed and not taken for deleted, and the other library's file is
/// converted.
#[tokio::test]
async fn a_job_that_fails_while_its_share_hangs_goes_back_to_the_queue() {
    let (app, file, job_id, share) =
        two_libraries(Behavior::HoldFail("ffmpeg couldn't read the file".into())).await;
    app.resume().await;
    wait("the share's job to start", async || {
        app.fake.running.load(Ordering::SeqCst) == 1
    })
    .await;
    let hung = hang::hang(&file);
    app.fake.release.add_permits(1);
    requeued_offline(&app, &job_id, &share).await;

    // Cancel still works on it, in the queue.
    let r = app.post_empty(&format!("/api/jobs/{job_id}/cancel")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["state"], "cancelled", "{}", r.json);
    drop(hung);
}

/// Cancel while a failed job's last look at its share waits: the job ends
/// cancelled at once (not failed, and not back in the queue).
#[tokio::test]
async fn cancel_wins_while_a_failed_job_looks_at_its_share() {
    let (app, file, job_id, _share) =
        two_libraries(Behavior::HoldFail("ffmpeg couldn't read the file".into())).await;
    app.resume().await;
    wait("the share's job to start", async || {
        app.fake.running.load(Ordering::SeqCst) == 1
    })
    .await;
    // The whole share hangs: the look at the file and at the library
    // folder both wait.
    let hung = hang::hang(file.parent().unwrap());
    app.fake.release.add_permits(1);
    wait("the converter to have given up", async || {
        app.fake.running.load(Ordering::SeqCst) == 0
    })
    .await;
    let started = Instant::now();
    let r = app.post_empty(&format!("/api/jobs/{job_id}/cancel")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["state"], "cancelled", "{}", r.json);
    assert!(started.elapsed() < Duration::from_secs(30));
    let files = app.get("/api/files?status=failed").await;
    assert_eq!(files.json["total"], 0, "{}", files.json);
    drop(hung);
}

/// A cancel recorded while the job was going back to the queue (its share
/// stopped answering in the same moment) wins: the job ends cancelled, and
/// its library isn't marked offline for it.
#[tokio::test]
async fn a_recorded_cancel_wins_over_going_back_to_the_queue() {
    let (app, _file, job_id, share) = two_libraries(Behavior::StuckThenNotResponding).await;
    app.resume().await;
    wait("the share's job to start", async || {
        app.fake.running.load(Ordering::SeqCst) == 1
    })
    .await;
    // The converter is stuck (it doesn't hear Cancel) when Cancel comes.
    let cancel = tokio::spawn({
        let router = app.router.clone();
        let id = job_id.clone();
        async move { cancel_via(router, &id).await }
    });
    wait("the cancel to be recorded", async || {
        app.state.dispatcher.intent_of(uuid(&job_id)) == Some(CancelIntent::User)
    })
    .await;
    // Now it gives up because the share stopped answering.
    app.fake.release.add_permits(1);
    let (status, body) = cancel.await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], "cancelled", "{body}");
    let j = job(&app, &job_id).await;
    assert_eq!(j["state"], "cancelled", "{j}");
    assert!(app.state.dispatcher.offline_reason(uuid(&share)).is_none());
    let pending = app.get("/api/files?status=pending").await;
    assert_eq!(pending.json["total"], 1, "{}", pending.json);
}

/// `POST /api/jobs/{id}/cancel` through the router.
async fn cancel_via(router: axum::Router, id: &str) -> (StatusCode, Value) {
    use tower::ServiceExt;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri(format!("/api/jobs/{id}/cancel"))
        .body(axum::body::Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The converter reports that the share stopped answering (during the
/// encode, the checks or a look at a folder): back in the queue, library
/// offline, the other library goes on.
#[tokio::test]
async fn a_job_whose_share_stops_answering_goes_back_to_the_queue() {
    let (app, _file, job_id, share) = two_libraries(Behavior::NotResponding).await;
    app.resume().await;
    requeued_offline(&app, &job_id, &share).await;
    let act = app.get("/api/activity").await;
    assert!(
        act.text.contains("can't be reached right now"),
        "{}",
        act.text
    );
}

/// The file changed, so the job probes it again, and ffprobe hangs on the
/// share: Cancel ends the job within seconds; and when the file itself
/// stops answering meanwhile, the job goes back to the queue with its
/// library offline instead of waiting for ffprobe's own timeout.
#[tokio::test]
async fn a_probe_on_a_hung_share_gives_way() {
    let (app, file, job_id, share) = two_libraries(Behavior::Done { ratio: 0.5 }).await;
    // New content since the scan: probed again when the job starts.
    std::fs::write(&file, format!("{}{}", h264(), "x".repeat(64))).unwrap();
    app.fake
        .probe_hold
        .lock()
        .unwrap()
        .insert("stuck.mkv".into());
    let probes = app.fake.probes.load(Ordering::SeqCst);
    app.resume().await;
    wait("the probe to start", async || {
        app.fake.probes.load(Ordering::SeqCst) > probes
    })
    .await;
    let started = Instant::now();
    let r = app.post_empty(&format!("/api/jobs/{job_id}/cancel")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["state"], "cancelled", "{}", r.json);
    assert!(started.elapsed() < Duration::from_secs(30));
    wait("the slot to be free", async || {
        app.state.dispatcher.running_count() == 0
    })
    .await;

    // Again, and this time the file stops answering while ffprobe waits.
    app.pause().await;
    let file_id = job(&app, &job_id).await["file_id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = app.post_empty(&format!("/api/files/{file_id}/queue")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let again = r.json["id"].as_str().unwrap().to_string();
    let probes = app.fake.probes.load(Ordering::SeqCst);
    app.resume().await;
    wait("the probe to start", async || {
        app.fake.probes.load(Ordering::SeqCst) > probes
    })
    .await;
    let hung = hang::hang(&file);
    wait("the job to go back to the queue", async || {
        job(&app, &again).await["state"] == "queued"
    })
    .await;
    assert!(
        app.state
            .dispatcher
            .offline_reason(uuid(&share))
            .is_some_and(|r| r.contains("isn't responding"))
    );
    app.fake.probe_hold.lock().unwrap().clear();
    drop(hung);
}

/// The share stopped answering while the new file was being put in place:
/// the job goes back to the queue at once (its slot free), the file isn't
/// started again while that step may still go on, and when it ends the
/// job is recorded as it ended: done when the new file took its place.
#[tokio::test]
async fn a_new_file_still_moving_in_is_followed_to_its_end() {
    let (app, _file, job_id, share) = two_libraries(Behavior::StuckPlacing).await;
    app.resume().await;
    requeued_offline(&app, &job_id, &share).await;
    let id = uuid(&job_id);
    assert!(app.state.dispatcher.is_placing(id));
    let act = app.get("/api/activity").await;
    assert!(
        act.text
            .contains("was being put in place when its folder stopped answering"),
        "{}",
        act.text
    );

    // The share is back, but the file waits for that step to end.
    app.state.dispatcher.library_back(uuid(&share));
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(job(&app, &job_id).await["state"], "queued");
    let runs = app
        .fake
        .started()
        .iter()
        .filter(|n| *n == "stuck.mkv")
        .count();
    assert_eq!(runs, 1, "not started again");

    // It ends: the new file took its place.
    app.fake.finish_placing(id, true).await;
    wait("the job to be recorded as done", async || {
        job(&app, &job_id).await["state"] == "done"
    })
    .await;
    let j = job(&app, &job_id).await;
    assert!(
        j["notes"]
            .to_string()
            .contains("finished when the folder answered again"),
        "{j}"
    );
    assert!(!app.state.dispatcher.is_placing(id));
    wait("both files to be done", async || {
        app.get("/api/files?status=done").await.json["total"] == 2
    })
    .await;
    let runs = app
        .fake
        .started()
        .iter()
        .filter(|n| *n == "stuck.mkv")
        .count();
    assert_eq!(runs, 1, "converted once");
}

/// Cancel a job whose new file is still being put in place on a share that
/// stopped answering: it ends cancelled at once, and that step is asked to
/// undo itself. Undone, the job stays cancelled and the file can be queued
/// again; too late to undo, the job is recorded as done (what happened to
/// the file), with a note.
#[tokio::test]
async fn cancelling_a_new_file_still_moving_in() {
    for placed in [false, true] {
        let (app, _file, job_id, share) = two_libraries(Behavior::StuckPlacing).await;
        app.resume().await;
        requeued_offline(&app, &job_id, &share).await;
        let id = uuid(&job_id);
        let r = app.post_empty(&format!("/api/jobs/{job_id}/cancel")).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(r.json["state"], "cancelled", "{}", r.json);
        assert!(app.fake.placing_stopped(id), "asked to undo");

        app.fake.finish_placing(id, placed).await;
        wait("the step to be over", async || {
            !app.state.dispatcher.is_placing(id)
        })
        .await;
        let j = job(&app, &job_id).await;
        if placed {
            assert_eq!(j["state"], "done", "{j}");
            assert!(j["notes"].to_string().contains("too far to undo"), "{j}");
        } else {
            assert_eq!(j["state"], "cancelled", "{j}");
            let file_id = j["file_id"].as_str().unwrap();
            let f = app.get(&format!("/api/files/{file_id}")).await.json;
            assert_eq!(f["file"]["status"], "pending", "{f}");
        }
    }
}

/// An earlier run of a job stopped while its new file was being put in
/// place (the server stopped while the share hung): when the job runs
/// again, the original's backup next to the new file shows the new file
/// was complete, so the job is recorded as done instead of converting the
/// new file again, and the backup goes.
#[tokio::test]
async fn a_job_whose_earlier_run_put_its_file_in_place_is_finished() {
    let app = TestApp::new().await;
    app.pause().await;
    let file = app.write("Movies/film.mkv", h264());
    app.add_library("Movies", json!({})).await;
    let file = std::fs::canonicalize(&file).unwrap();
    let job_id = job_of(&app, "film.mkv").await;
    // What the earlier run left: where its result went, the new file there
    // and the original as its backup.
    sqlx::query("UPDATE jobs SET final_path = ? WHERE id = ?")
        .bind(file.to_str().unwrap())
        .bind(&job_id)
        .execute(app.state.db.pool())
        .await
        .unwrap();
    let backup = file.with_file_name(szalinski_core::paths::backup_file_name(
        "film.mkv",
        uuid(&job_id),
    ));
    std::fs::rename(&file, &backup).unwrap();
    std::fs::write(&file, "video=hevc\naudio=aac\nwidth=1920\nheight=1080\n").unwrap();

    app.resume().await;
    wait("the job to be recorded as done", async || {
        job(&app, &job_id).await["state"] == "done"
    })
    .await;
    assert!(app.fake.started().is_empty(), "not converted again");
    assert!(!backup.exists(), "the backup went");
    assert!(file.exists());
    let j = job(&app, &job_id).await;
    assert!(j["notes"].to_string().contains("already complete"), "{j}");
}

/// The file a job starts on doesn't answer, and it isn't the library folder
/// that hangs: the library waits until that file answers too, not just its
/// folder.
#[tokio::test]
async fn a_library_waits_for_what_stopped_answering() {
    let (app, file, job_id, share) = two_libraries(Behavior::Done { ratio: 0.5 }).await;
    let hung = hang::hang(&file);
    app.resume().await;
    requeued_offline(&app, &job_id, &share).await;
    // Checked again (as every 15 s): the folder answers, the file doesn't.
    force_recheck(&app, &share);
    app.state.dispatcher.wake();
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(app.state.dispatcher.offline_reason(uuid(&share)).is_some());
    drop(hung);
    force_recheck(&app, &share);
    app.state.dispatcher.wake();
    wait("the share's file to be converted", async || {
        job(&app, &job_id).await["state"] == "done"
    })
    .await;
}

fn force_recheck(app: &TestApp, share: &str) {
    app.state.dispatcher.recheck_now(uuid(share));
}

/// Hung files and folders cost one stuck thread each, however often the
/// queue comes back to them.
#[test]
fn hung_paths_are_matched_by_prefix() {
    let root = Path::new("/srv/share-for-tests");
    let _hung = hang::hang(root);
    assert!(hang::is_hung(&root.join("a/b.mkv")));
    assert!(!hang::is_hung(Path::new("/srv/share-for-tests-2/a.mkv")));
}
