//! Round 4: every failure records what kind of problem it is, on the job
//! and on the file, in API responses and in events; the kind goes away when
//! the file stops being failed.

use std::sync::Arc;

use chrysopoeia_core::{Event, ProblemKind};
use serde_json::{Value, json};

use super::support::*;

fn id_of(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

/// Every kind, as the API spells it.
const KINDS: [(ProblemKind, &str); 9] = [
    (ProblemKind::UnreadableSource, "unreadable_source"),
    (ProblemKind::WorkFolder, "work_folder"),
    (ProblemKind::Destination, "destination"),
    (ProblemKind::DiskFull, "disk_full"),
    (ProblemKind::Encoder, "encoder"),
    (ProblemKind::HardwareUnavailable, "hardware_unavailable"),
    (ProblemKind::Verification, "verification"),
    (ProblemKind::SourceChanged, "source_changed"),
    (ProblemKind::Other, "other"),
];

#[tokio::test]
async fn every_kind_of_failure_is_recorded_on_the_job_the_file_and_in_events() {
    let fake = Arc::new(FakeToolkit::default());
    let app = TestApp::start(TestOptions {
        fake: Arc::clone(&fake),
        ..TestOptions::default()
    })
    .await;
    let mut events = app.state.events.subscribe();
    for (kind, name) in KINDS {
        app.write(&format!("Movies/{name}.mkv"), h264());
        fake.set_behavior(
            &format!("{name}.mkv"),
            Behavior::FailWith(kind, format!("It failed ({name}).")),
        );
    }
    let lib = app.add_library("Movies", json!({})).await;
    app.wait_queue_idle().await;

    let files = app.files_by_name(&id_of(&lib)).await;
    for (_, name) in KINDS {
        let file = &files[&format!("{name}.mkv")];
        assert_eq!(file["status"], "failed", "{file}");
        assert_eq!(file["problem"], name, "{file}");
        assert_eq!(file["error"], format!("It failed ({name})."), "{file}");
        let detail = app.get(&format!("/api/files/{}", id_of(file))).await.json;
        assert_eq!(detail["file"]["problem"], name);
        let job = &detail["jobs"][0];
        assert_eq!(job["state"], "failed");
        assert_eq!(job["problem"], name, "{job}");
        let job = app.get(&format!("/api/jobs/{}", id_of(job))).await.json;
        assert_eq!(job["problem"], name, "{job}");
    }
    let history = app.get("/api/jobs?state=history").await.json;
    for job in history["items"].as_array().unwrap() {
        assert!(job["problem"].is_string(), "{job}");
    }

    // The events the UI updates from carry the kind too.
    let mut job_kinds = Vec::new();
    let mut file_kinds = Vec::new();
    loop {
        let event = match events.try_recv() {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        };
        match event {
            Event::JobUpdated { job } if job.error.is_some() => job_kinds.push(job.problem),
            Event::FileUpdated { file } if file.error.is_some() => file_kinds.push(file.problem),
            Event::JobUpdated { job } => assert_eq!(job.problem, None, "{job:?}"),
            Event::FileUpdated { file } => assert_eq!(file.problem, None, "{file:?}"),
            _ => {}
        }
    }
    for (kind, name) in KINDS {
        assert!(job_kinds.contains(&Some(kind)), "job.updated for {name}");
        assert!(file_kinds.contains(&Some(kind)), "file.updated for {name}");
    }
}

#[tokio::test]
async fn the_problem_goes_away_when_the_file_stops_being_failed() {
    let fake = Arc::new(FakeToolkit::default());
    let app = TestApp::start(TestOptions {
        fake: Arc::clone(&fake),
        ..TestOptions::default()
    })
    .await;
    app.write("Movies/a.mkv", h264());
    app.write("Movies/b.mkv", h264());
    for name in ["a.mkv", "b.mkv"] {
        fake.set_behavior(
            name,
            Behavior::FailWith(ProblemKind::DiskFull, "The disk is full.".into()),
        );
    }
    let lib = app.add_library("Movies", json!({})).await;
    app.wait_queue_idle().await;
    let files = app.files_by_name(&id_of(&lib)).await;
    let (a, b) = (id_of(&files["a.mkv"]), id_of(&files["b.mkv"]));
    assert_eq!(files["a.mkv"]["problem"], "disk_full");

    // Queued again: no problem while it waits, none once it converts.
    app.pause().await;
    fake.set_behavior("a.mkv", Behavior::Done { ratio: 0.5 });
    let job = app.post_empty(&format!("/api/files/{a}/queue")).await.json;
    assert!(job["problem"].is_null(), "{job}");
    let file = app.get(&format!("/api/files/{a}")).await.json["file"].clone();
    assert_eq!(file["status"], "queued");
    assert!(
        file["problem"].is_null() && file["error"].is_null(),
        "{file}"
    );
    app.resume().await;
    app.wait_queue_idle().await;
    let detail = app.get(&format!("/api/files/{a}")).await.json;
    assert_eq!(detail["file"]["status"], "done");
    assert!(detail["file"]["problem"].is_null(), "{}", detail["file"]);
    // The failed attempt keeps its record.
    assert_eq!(detail["jobs"][1]["state"], "failed");
    assert_eq!(detail["jobs"][1]["problem"], "disk_full");

    // Skipped by hand: no longer failed, so no problem either.
    let file = app.post_empty(&format!("/api/files/{b}/skip")).await.json;
    assert_eq!(file["status"], "skipped");
    assert!(
        file["problem"].is_null() && file["error"].is_null(),
        "{file}"
    );
}

#[tokio::test]
async fn failures_found_by_the_server_itself_have_their_kinds() {
    let app = TestApp::new().await;
    app.pause().await;
    // Not a video at all: found at scan time.
    app.write("Movies/broken.mkv", "broken");
    // Gone before its job ran (watching off, so the row stays).
    app.patch("/api/settings", json!({ "watch_folders": false }))
        .await;
    let gone = app.write("Movies/gone.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let files = app.files_by_name(&id_of(&lib)).await;
    assert_eq!(files["broken.mkv"]["status"], "failed");
    assert_eq!(files["broken.mkv"]["problem"], "unreadable_source");
    std::fs::remove_file(&gone).unwrap();
    app.resume().await;
    app.wait_queue_idle().await;
    let files = app.files_by_name(&id_of(&lib)).await;
    let file = &files["gone.mkv"];
    assert_eq!(file["status"], "failed", "{file}");
    assert_eq!(file["problem"], "source_changed", "{file}");
    let job = app
        .get(&format!(
            "/api/jobs/{}",
            id_of(&json!({"id": file["job_id"]}))
        ))
        .await;
    assert_eq!(job.json["problem"], "source_changed", "{}", job.text);
}

#[tokio::test]
async fn hardware_that_cant_be_used_is_hardware_unavailable() {
    let app = TestApp::new().await;
    let r = app
        .patch(
            "/api/settings",
            json!({ "hardware": "nvenc", "cpu_fallback": false }),
        )
        .await;
    assert_eq!(r.status, axum::http::StatusCode::OK, "{}", r.text);
    app.write("Movies/new.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    app.wait_queue_idle().await;
    let file = app.files_by_name(&id_of(&lib)).await["new.mkv"].clone();
    assert_eq!(file["status"], "failed", "{file}");
    assert_eq!(file["problem"], "hardware_unavailable", "{file}");
}

#[tokio::test]
async fn a_failed_reconversion_keeps_the_file_done_without_a_problem() {
    let fake = Arc::new(FakeToolkit::default());
    let app = TestApp::start(TestOptions {
        fake: Arc::clone(&fake),
        ..TestOptions::default()
    })
    .await;
    app.write("Movies/a.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    app.wait_queue_idle().await;
    let before = app.files_by_name(&id_of(&lib)).await["a.mkv"].clone();
    assert_eq!(before["status"], "done");
    fake.set_behavior(
        "a.mkv",
        Behavior::FailWith(ProblemKind::Destination, "Read-only.".into()),
    );
    let id = id_of(&before);
    let job = app
        .post(&format!("/api/files/{id}/queue"), json!({ "force": true }))
        .await
        .json;
    app.wait_queue_idle().await;
    let job = app.get(&format!("/api/jobs/{}", id_of(&job))).await.json;
    assert_eq!(job["state"], "failed");
    assert_eq!(job["problem"], "destination");
    let file = app.get(&format!("/api/files/{id}")).await.json["file"].clone();
    assert_eq!(file["status"], "done", "{file}");
    assert!(
        file["problem"].is_null() && file["error"].is_null(),
        "{file}"
    );
    assert_eq!(file["saved_bytes"], before["saved_bytes"]);
}
