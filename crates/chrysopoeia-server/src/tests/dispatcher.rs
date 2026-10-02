//! The dispatcher: ordering, limits, schedule and outcomes.

use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use chrono::Timelike;
use serde_json::json;

use super::support::*;

async fn queued_job_ids(app: &TestApp) -> Vec<(String, String)> {
    app.get("/api/jobs?state=queued").await.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| {
            (
                j["id"].as_str().unwrap().to_string(),
                j["file_name"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

#[tokio::test]
async fn runs_by_priority_then_age() {
    let app = TestApp::new().await;
    app.pause().await;
    app.patch("/api/settings", json!({ "max_jobs": 1 })).await;
    for n in ["one", "two", "three"] {
        app.write(&format!("Movies/{n}.mkv"), h264());
    }
    app.add_library("Movies", json!({})).await;
    let jobs = queued_job_ids(&app).await;
    let by_name = |name: &str| jobs.iter().find(|(_, n)| n == name).unwrap().0.clone();
    app.post(
        &format!("/api/jobs/{}/priority", by_name("three.mkv")),
        json!({ "priority": 10 }),
    )
    .await;
    app.post(
        &format!("/api/jobs/{}/priority", by_name("two.mkv")),
        json!({ "priority": 5 }),
    )
    .await;
    app.resume().await;
    app.wait_queue_idle().await;
    assert_eq!(app.fake.started(), ["three.mkv", "two.mkv", "one.mkv"]);
    assert_eq!(app.fake.max_running.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn max_jobs_is_respected_and_changes_live() {
    let app = TestApp::new().await;
    app.fake.set_default(Behavior::Hold);
    for n in 0..5 {
        app.write(&format!("Movies/{n}.mkv"), h264());
    }
    app.add_library("Movies", json!({})).await;
    let app_ref = &app;
    // Automatic: the fake hardware recommends 2.
    wait_until("2 running", || async {
        app_ref.fake.running.load(Ordering::SeqCst) == 2
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(app.fake.running.load(Ordering::SeqCst), 2);

    let r = app.patch("/api/settings", json!({ "max_jobs": 3 })).await;
    assert_eq!(r.status, StatusCode::OK);
    wait_until("3 running", || async {
        app_ref.fake.running.load(Ordering::SeqCst) == 3
    })
    .await;
    let q = app.get("/api/queue").await;
    assert_eq!(q.json["max_jobs"], 3);
    assert_eq!(q.json["max_jobs_auto"], false);
    assert_eq!(q.json["running"], 3);
    assert_eq!(q.json["queued"], 2);

    // Shrinking never kills running jobs.
    app.patch("/api/settings", json!({ "max_jobs": 1 })).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(app.fake.running.load(Ordering::SeqCst), 3);
    // Finish them one at a time: only one runs from now on.
    app.fake.release.add_permits(3);
    wait_until("drain to 1", || async {
        app_ref.fake.running.load(Ordering::SeqCst) == 1
            && app_ref.get("/api/queue").await.json["queued"] == 1
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(app.fake.running.load(Ordering::SeqCst), 1);
    assert_eq!(app.fake.max_running.load(Ordering::SeqCst), 3);
    app.fake.release.add_permits(10);
    app.wait_queue_idle().await;
    assert_eq!(app.get("/api/files?status=done").await.json["total"], 5);
}

#[tokio::test]
async fn paused_queue_starts_nothing() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/a.mkv", h264());
    app.add_library("Movies", json!({})).await;
    let app_ref = &app;
    wait_until("the job to be queued", || async {
        app_ref.get("/api/queue").await.json["queued"] == 1
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(app.fake.started().is_empty());
    let q = app.get("/api/queue").await;
    assert_eq!(q.json["paused"], true);
    assert_eq!(q.json["queued"], 1);
    assert_eq!(q.json["waiting_for_schedule"], false);
}

#[tokio::test]
async fn active_hours_hold_jobs() {
    let app = TestApp::new().await;
    let hour = chrono::Local::now().hour();
    // A one-hour window that excludes the current hour.
    let start = (hour + 2) % 24;
    let end = (hour + 3) % 24;
    let r = app
        .patch(
            "/api/settings",
            json!({ "active_hours": { "start": start, "end": end } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.write("Movies/a.mkv", h264());
    app.add_library("Movies", json!({})).await;
    let app_ref = &app;
    wait_until("the queue to wait for its hours", || async {
        app_ref.get("/api/queue").await.json["waiting_for_schedule"] == true
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(app.fake.started().is_empty());

    // Opening the window starts the job right away.
    app.patch("/api/settings", json!({ "active_hours": null }))
        .await;
    app.wait_queue_idle().await;
    assert_eq!(app.fake.started(), ["a.mkv"]);
}

#[tokio::test]
async fn done_outcome_moves_to_new_extension_and_records_savings() {
    let app = TestApp::new().await;
    app.write(
        "Movies/Big Test (2020).mp4",
        &format!("{}{}", h264(), "p".repeat(900)),
    );
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib_id = lib["id"].as_str().unwrap().to_string();
    app.wait_queue_idle().await;

    let files = app.files_by_name(&lib_id).await;
    assert_eq!(files.len(), 1);
    let f = &files["Big Test (2020).mkv"];
    assert_eq!(f["status"], "done");
    assert_eq!(f["relative_path"], "Big Test (2020).mkv");
    assert!(f["path"].as_str().unwrap().ends_with("Big Test (2020).mkv"));
    assert_eq!(f["video_codec"], "av1", "re-probed after the replace");
    let original = f["original_size_bytes"].as_u64().unwrap();
    let size = f["size_bytes"].as_u64().unwrap();
    assert!(
        (size as i64 - (original / 2) as i64).abs() <= 1,
        "{size} vs {original}"
    );
    assert_eq!(f["saved_bytes"].as_i64().unwrap(), (original - size) as i64);
    assert!(!app.media.join("Movies/Big Test (2020).mp4").exists());
    assert!(app.media.join("Movies/Big Test (2020).mkv").exists());

    let job = app
        .get(&format!("/api/jobs/{}", f["job_id"].as_str().unwrap()))
        .await;
    assert_eq!(job.json["state"], "done");
    assert_eq!(job.json["progress"], 100.0);
    assert_eq!(job.json["encoder"], "libsvtav1");
    assert_eq!(job.json["hw_api"], "software");
    assert_eq!(job.json["validation"]["passed"], true);
    assert_eq!(job.json["output_size"].as_u64().unwrap(), size);
    assert!(job.json["finished_at"].is_string());

    let ov = app.get("/api/overview").await;
    let history = ov.json["savings_history"].as_array().unwrap();
    assert_eq!(history.len(), 30);
    let today = history.last().unwrap();
    assert_eq!(today["files"], 1);
    assert_eq!(
        today["saved_bytes"].as_i64().unwrap(),
        (original - size) as i64
    );
    assert_eq!(ov.json["totals"]["done"], 1);
    assert_eq!(
        ov.json["totals"]["saved_bytes"].as_i64().unwrap(),
        (original - size) as i64
    );
    assert!(
        ov.json["projected_savings_bytes"].is_null(),
        "needs 3 done files"
    );

    let act = app.get("/api/activity").await;
    let msg = act.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["level"] == "success")
        .unwrap()["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        msg.starts_with("Converted Big Test (2020).mp4 — saved"),
        "{msg}"
    );
    assert!(msg.ends_with("(50%), verified"), "{msg}");

    // A rescan sees the replaced file as unchanged.
    let probes = app.fake.probes.load(Ordering::SeqCst);
    app.rescan(&lib_id).await;
    assert_eq!(app.fake.probes.load(Ordering::SeqCst), probes);
    assert_eq!(
        app.files_by_name(&lib_id).await["Big Test (2020).mkv"]["status"],
        "done"
    );
}

#[tokio::test]
async fn skipped_failed_and_panicking_outcomes() {
    let app = TestApp::new().await;
    app.fake.set_behavior(
        "small.mkv",
        Behavior::Skip("Only 3% smaller — kept the original".into()),
    );
    app.fake.set_behavior(
        "bad.mkv",
        Behavior::Fail("The encoder stopped: invalid frame size".into()),
    );
    app.fake.set_behavior("boom.mkv", Behavior::Panic);
    for n in ["small", "bad", "boom"] {
        app.write(&format!("Movies/{n}.mkv"), h264());
    }
    let lib = app.add_library("Movies", json!({})).await;
    app.wait_queue_idle().await;
    let files = app.files_by_name(lib["id"].as_str().unwrap()).await;

    assert_eq!(files["small.mkv"]["status"], "skipped");
    assert_eq!(
        files["small.mkv"]["skip_reason"],
        "Only 3% smaller — kept the original"
    );
    assert_eq!(files["bad.mkv"]["status"], "failed");
    assert_eq!(
        files["bad.mkv"]["error"],
        "The encoder stopped: invalid frame size"
    );
    assert_eq!(files["boom.mkv"]["status"], "failed");
    assert!(
        files["boom.mkv"]["error"]
            .as_str()
            .unwrap()
            .contains("stopped unexpectedly")
    );

    let bad_job = app
        .get(&format!(
            "/api/jobs/{}",
            files["bad.mkv"]["job_id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(bad_job.json["state"], "failed");
    assert_eq!(bad_job.json["attempt"], 2);
    assert_eq!(bad_job.json["log_tail"], "Error while decoding stream #0:0");
    let small_job = app
        .get(&format!(
            "/api/jobs/{}",
            files["small.mkv"]["job_id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(small_job.json["state"], "skipped");
    assert_eq!(
        small_job.json["skip_reason"],
        "Only 3% smaller — kept the original"
    );

    let act = app.get("/api/activity").await;
    let messages: Vec<String> = act.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["message"].as_str().unwrap().to_string())
        .collect();
    assert!(
        messages.contains(&"Skipped small.mkv: Only 3% smaller — kept the original".to_string())
    );
    assert!(
        messages.contains(&"Failed bad.mkv: The encoder stopped: invalid frame size".to_string())
    );
    // All originals untouched.
    for n in ["small", "bad", "boom"] {
        assert!(app.media.join(format!("Movies/{n}.mkv")).exists());
    }
    // The slot freed by the panic is reusable: retry works.
    app.fake
        .set_behavior("boom.mkv", Behavior::Done { ratio: 0.4 });
    let r = app
        .post_empty(&format!(
            "/api/files/{}/queue",
            files["boom.mkv"]["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    app.wait_queue_idle().await;
    assert_eq!(app.get("/api/files?status=done").await.json["total"], 1);
}

#[tokio::test]
async fn folder_mode_keeps_originals_and_passes_settings() {
    let app = TestApp::new().await;
    let out = app.dir.path().join("out");
    let tmp = app.dir.path().join("tmp");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::create_dir_all(&tmp).unwrap();
    let r = app
        .patch(
            "/api/settings",
            json!({
                "output_mode": "folder",
                "output_folder": out.to_str().unwrap(),
                "temp_dir": tmp.to_str().unwrap(),
                "validation": "thorough",
                "low_priority": false,
            }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.write("Movies/Sub/a.mp4", h264());
    let lib = app.add_library("Movies", json!({})).await;
    app.wait_queue_idle().await;
    let files = app.files_by_name(lib["id"].as_str().unwrap()).await;
    let f = &files["a.mp4"];
    assert_eq!(f["status"], "done");
    assert!(app.media.join("Movies/Sub/a.mp4").exists());
    assert!(out.join("Sub/a.mkv").exists());
    assert!(f["saved_bytes"].as_i64().unwrap() > 0);
    let cfg = app.fake.run_configs.lock().unwrap()[0].clone();
    assert_eq!(cfg.temp_dir.as_deref(), Some(tmp.as_path()));
    assert_eq!(cfg.output_folder.as_deref(), Some(out.as_path()));
    assert_eq!(cfg.validation, chrysopoeia_core::ValidationLevel::Thorough);
    assert!(!cfg.low_priority);
}

#[tokio::test]
async fn projected_savings_after_three_files() {
    let app = TestApp::new().await;
    app.pause().await;
    for n in 0..4 {
        app.write(
            &format!("Movies/{n}.mkv"),
            &format!("{}{}", h264(), "z".repeat(400)),
        );
    }
    app.add_library("Movies", json!({})).await;
    let jobs = queued_job_ids(&app).await;
    // Leave one queued, run three.
    app.post_empty(&format!("/api/jobs/{}/cancel", jobs[3].0))
        .await;
    app.resume().await;
    app.wait_queue_idle().await;
    let ov = app.get("/api/overview").await;
    assert_eq!(ov.json["totals"]["done"], 3);
    assert_eq!(ov.json["totals"]["pending"], 1);
    let pending_bytes = app.get("/api/files?status=pending").await.json["items"][0]["size_bytes"]
        .as_i64()
        .unwrap();
    let projected = ov.json["projected_savings_bytes"].as_i64().unwrap();
    assert!(
        (projected - pending_bytes / 2).abs() <= 2,
        "{projected} vs {pending_bytes}"
    );
    let codecs = ov.json["video_codecs"].as_array().unwrap();
    assert!(codecs.iter().any(|c| c["name"] == "av1" && c["files"] == 3));
    assert!(
        codecs
            .iter()
            .any(|c| c["name"] == "h264" && c["files"] == 1)
    );
    assert_eq!(ov.json["resolutions"][0]["name"], "1080p");
}

/// A share that stops answering: the job starting on one of its files used
/// to stay "preparing" for good, holding its slot (every other library
/// waited) and ignoring Cancel. Now Cancel ends it at once, and when the
/// file doesn't answer in time the job goes back to the queue with its
/// library marked offline, so other libraries' files are converted.
#[tokio::test]
async fn a_file_on_a_share_that_stopped_answering_does_not_hold_the_queue() {
    use crate::services::fs_guard::hang;
    let app = TestApp::new().await;
    app.pause().await;
    app.patch("/api/settings", json!({ "max_jobs": 1 })).await;
    let stuck = app.write("Share/stuck.mkv", h264());
    app.write("Healthy/fine.mkv", h264());
    let share = app.add_library("Share", json!({})).await;
    let stuck = std::fs::canonicalize(&stuck).unwrap();
    // The share's file goes first.
    let job = app.get("/api/jobs?state=queued").await.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["file_name"] == "stuck.mkv")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    app.post(
        &format!("/api/jobs/{job}/priority"),
        json!({ "priority": 10 }),
    )
    .await;
    app.add_library("Healthy", json!({})).await;

    // Cancel works while the file doesn't answer.
    let hung = hang::hang(&stuck);
    app.resume().await;
    let state = app.state.clone();
    wait_until("the job starts", move || {
        let state = state.clone();
        async move { state.dispatcher.running_count() == 1 }
    })
    .await;
    let started = std::time::Instant::now();
    let r = app.post_empty(&format!("/api/jobs/{job}/cancel")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let state = app.state.clone();
    wait_until("the cancelled job lets go of its slot", move || {
        let state = state.clone();
        async move { state.dispatcher.running_count() == 0 }
    })
    .await;
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    let j = app.get(&format!("/api/jobs/{job}")).await;
    assert_eq!(j.json["state"], "cancelled", "{}", j.json);
    app.wait_queue_idle().await;
    assert_eq!(
        app.fake.started(),
        ["fine.mkv"],
        "the healthy library went on"
    );

    // Queued again while the share still hangs: back in the queue, the
    // library shown offline, nothing converted.
    app.pause().await;
    let r = app
        .post_empty(&format!(
            "/api/files/{}/queue",
            j.json["file_id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.resume().await;
    let share_id = share["id"].as_str().unwrap().to_string();
    wait_until("the share is shown offline", {
        let app_state = app.state.clone();
        move || {
            let state = app_state.clone();
            let id = share_id.clone();
            async move {
                crate::db::libraries::list(state.db.pool()).await.is_ok()
                    && state
                        .dispatcher
                        .offline_reason(uuid::Uuid::parse_str(&id).unwrap())
                        .is_some_and(|r| r.contains("isn't responding"))
            }
        }
    })
    .await;
    // The offline mark can land a moment before the slot is released.
    let state = app.state.clone();
    wait_until("the requeued job lets go of its slot", move || {
        let state = state.clone();
        async move { state.dispatcher.running_count() == 0 }
    })
    .await;
    let queued = app.get("/api/jobs?state=queued").await;
    assert_eq!(queued.json["total"], 1, "{}", queued.json);
    assert_eq!(app.fake.started(), ["fine.mkv"]);
    drop(hung);
}

/// A replaced original that has another hard link (a seeding torrent's
/// copy) frees no space: the overview and the chart count nothing saved.
#[cfg(unix)]
#[tokio::test]
async fn converting_a_hard_linked_original_saves_nothing() {
    let app = TestApp::new().await;
    app.pause().await;
    let file = app.write(
        "Movies/Movie (2020).mp4",
        &format!("{}{}", h264(), "p".repeat(900)),
    );
    let torrent = app.media.join("torrent-copy.mp4");
    std::fs::hard_link(&file, &torrent).unwrap();
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib_id = lib["id"].as_str().unwrap().to_string();
    app.resume().await;
    app.wait_queue_idle().await;

    let files = app.files_by_name(&lib_id).await;
    let f = &files["Movie (2020).mkv"];
    assert_eq!(f["status"], "done", "{f}");
    assert_eq!(f["saved_bytes"], 0, "{f}");
    let ov = app.get("/api/overview").await;
    assert_eq!(ov.json["totals"]["saved_bytes"], 0, "{}", ov.json);
    let today = ov.json["savings_history"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(today["saved_bytes"], 0, "{today}");
    let act = app.get("/api/activity").await;
    assert!(
        act.text.contains("Converted Movie (2020).mp4") && !act.text.contains("saved"),
        "{}",
        act.text
    );
    assert!(torrent.exists());
}

/// A job that runs out of disk space while another job writes to the same
/// disk is put back once and tried again (the space reservation makes it
/// wait its turn); alone, running out of space fails it as before.
#[tokio::test]
async fn a_disk_filled_by_parallel_jobs_is_retried_once() {
    use chrysopoeia_core::ProblemKind;
    let app = TestApp::new().await;
    app.pause().await;
    app.patch("/api/settings", json!({ "max_jobs": 1 })).await;
    app.write("Movies/long.mkv", h264());
    app.write("Movies/big.mkv", h264());
    let full = "The disk ran out of space while the new file was being written in the work folder.";
    app.fake.set_behavior("long.mkv", Behavior::Hold);
    app.fake.set_behavior(
        "big.mkv",
        Behavior::FailWith(ProblemKind::DiskFull, full.into()),
    );
    app.add_library("Movies", json!({})).await;
    // The long job runs first, so the disk fills while it runs (started
    // together, a busy machine could run the other one first, alone).
    wait_until("both jobs queued", || async {
        queued_job_ids(&app).await.len() == 2
    })
    .await;
    let jobs = queued_job_ids(&app).await;
    let long = jobs
        .iter()
        .find(|(_, n)| n == "long.mkv")
        .unwrap()
        .0
        .clone();
    app.post(
        &format!("/api/jobs/{long}/priority"),
        json!({ "priority": 10 }),
    )
    .await;
    app.resume().await;
    wait_until("long.mkv runs", || async {
        app.fake.started() == ["long.mkv"] && app.fake.running.load(Ordering::SeqCst) == 1
    })
    .await;
    app.patch("/api/settings", json!({ "max_jobs": 2 })).await;
    wait_until("big.mkv is put back", || async {
        app.fake
            .started()
            .iter()
            .filter(|n| *n == "big.mkv")
            .count()
            == 1
            && app.get("/api/jobs?state=queued").await.json["total"] == 1
    })
    .await;
    assert_eq!(app.get("/api/files?status=failed").await.json["total"], 0);
    // Tried again once the other job is done: this time it fits.
    app.fake
        .set_behavior("big.mkv", Behavior::Done { ratio: 0.5 });
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;
    assert_eq!(app.get("/api/files?status=done").await.json["total"], 2);

    // Alone on the disk, a full disk fails the file.
    app.pause().await;
    app.write("Movies/alone.mkv", h264());
    app.fake.set_behavior(
        "alone.mkv",
        Behavior::FailWith(ProblemKind::DiskFull, full.into()),
    );
    let lib = app.get("/api/libraries").await.json[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    app.rescan(&lib).await;
    app.resume().await;
    app.wait_queue_idle().await;
    let failed = app.get("/api/files?status=failed").await;
    assert_eq!(failed.json["total"], 1, "{}", failed.json);
    assert_eq!(failed.json["items"][0]["problem"], "disk_full");
}
