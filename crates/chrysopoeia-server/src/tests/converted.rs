//! Round 3 follow-ups: converted files that are queued again keep their
//! conversion whatever becomes of the new job, the chosen hardware only
//! stops files that need it (and waits for a busy GPU), bulk queueing leaves
//! out files the size rule already kept, and files still being copied are
//! counted from the folder watcher too.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::http::StatusCode;
use chrysopoeia_core::{EncoderStatus, HwApi, VideoCodec};
use serde_json::{Value, json};
use uuid::Uuid;

use super::support::*;
use crate::db;

fn id_of(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

fn uuid_of(v: &Value) -> Uuid {
    id_of(v).parse().unwrap()
}

/// Convert `Movies/a.mkv` and return the app, the library and the file.
async fn converted_file(fake: Arc<FakeToolkit>) -> (TestApp, Value, Value) {
    let app = TestApp::start(TestOptions {
        fake,
        ..TestOptions::default()
    })
    .await;
    app.write("Movies/a.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    app.wait_queue_idle().await;
    let file = app.files_by_name(&id_of(&lib)).await["a.mkv"].clone();
    assert_eq!(file["status"], "done", "{file}");
    assert!(file["saved_bytes"].as_i64().unwrap() > 0);
    (app, lib, file)
}

async fn file_detail(app: &TestApp, id: &str) -> Value {
    app.get(&format!("/api/files/{id}")).await.json
}

/// The file is still the converted one: done, same savings, pointing at the
/// conversion that made it.
async fn assert_still_converted(app: &TestApp, lib: &Value, before: &Value, what: &str) {
    let id = id_of(before);
    let detail = file_detail(app, &id).await;
    let file = &detail["file"];
    assert_eq!(file["status"], "done", "{what}: {file}");
    assert_eq!(file["saved_bytes"], before["saved_bytes"], "{what}: {file}");
    assert_eq!(file["job_id"], before["job_id"], "{what}: {file}");
    let stats = app
        .get(&format!("/api/libraries/{}", id_of(lib)))
        .await
        .json["stats"]
        .clone();
    assert_eq!(stats["done"], 1, "{what}: {stats}");
    assert_eq!(stats["pending"], 0, "{what}: {stats}");
    assert_eq!(stats["failed"], 0, "{what}: {stats}");
}

#[tokio::test]
async fn a_converted_file_stays_done_when_its_new_job_is_cancelled_or_fails() {
    let fake = Arc::new(FakeToolkit::default());
    let (app, lib, before) = converted_file(Arc::clone(&fake)).await;
    let id = id_of(&before);

    // Taken out of the queue before it ran.
    app.pause().await;
    let job = app
        .post(&format!("/api/files/{id}/queue"), json!({ "force": true }))
        .await
        .json;
    let r = app
        .post_empty(&format!("/api/jobs/{}/cancel", id_of(&job)))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["state"], "cancelled");
    assert_still_converted(&app, &lib, &before, "removed from the queue").await;

    // Cancelled while converting.
    fake.set_behavior("a.mkv", Behavior::Hold);
    let job = app
        .post(&format!("/api/files/{id}/queue"), json!({ "force": true }))
        .await
        .json;
    app.resume().await;
    wait_until("the job to run", || async {
        app.get("/api/jobs?state=running").await.json["total"] == 1
    })
    .await;
    let r = app
        .post_empty(&format!("/api/jobs/{}/cancel", id_of(&job)))
        .await;
    assert_eq!(r.json["state"], "cancelled", "{}", r.text);
    assert_still_converted(&app, &lib, &before, "cancelled while running").await;

    // Failed.
    fake.set_behavior("a.mkv", Behavior::Fail("The encoder stopped.".into()));
    let job = app.post_empty(&format!("/api/files/{id}/queue")).await.json;
    app.wait_queue_idle().await;
    assert_eq!(
        app.get(&format!("/api/jobs/{}", id_of(&job))).await.json["state"],
        "failed"
    );
    assert_still_converted(&app, &lib, &before, "failed").await;
    let feed = app.get("/api/activity").await.json["items"].to_string();
    assert!(
        feed.contains(
            "Failed a.mkv: The encoder stopped. The file stays as its earlier conversion left it."
        ),
        "{feed}"
    );
    // Every attempt is in the file's history.
    let states: Vec<String> = file_detail(&app, &id).await["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["state"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(states, ["failed", "cancelled", "cancelled", "done"]);
}

#[tokio::test]
async fn a_skipped_requeue_points_the_file_back_at_its_conversion() {
    let fake = Arc::new(FakeToolkit::default());
    let (app, lib, before) = converted_file(Arc::clone(&fake)).await;
    let id = id_of(&before);
    let done_job = before["job_id"].as_str().unwrap().to_string();
    // The conversion is old enough to be trimmed if nothing kept it.
    sqlx::query("UPDATE jobs SET finished_at = '2020-01-01T00:00:00.000Z' WHERE id = ?")
        .bind(&done_job)
        .execute(app.state.db.pool())
        .await
        .unwrap();

    // While the file is queued again it points at the new job; trimming
    // keeps the conversion anyway.
    app.pause().await;
    let r = app.post_empty(&format!("/api/files/{id}/queue")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    db::jobs::trim_history(&app.state.db).await.unwrap();
    assert!(
        db::jobs::get(app.state.db.pool(), done_job.parse().unwrap())
            .await
            .unwrap()
            .is_some(),
        "the conversion was trimmed while the file was queued"
    );

    fake.set_behavior("a.mkv", Behavior::Skip("Already AV1".into()));
    app.resume().await;
    app.wait_queue_idle().await;
    assert_still_converted(&app, &lib, &before, "skipped").await;
    // Now the file points at its conversion again, which trimming keeps.
    db::jobs::trim_history(&app.state.db).await.unwrap();
    let jobs = file_detail(&app, &id).await["jobs"].clone();
    assert!(
        jobs.as_array()
            .unwrap()
            .iter()
            .any(|j| j["id"] == done_job.as_str()),
        "{jobs}"
    );
}

#[tokio::test]
async fn new_content_in_a_converted_file_clears_its_old_savings() {
    let fake = Arc::new(FakeToolkit::default());
    let (app, lib, before) = converted_file(Arc::clone(&fake)).await;
    let id = id_of(&before);
    app.pause().await;
    let r = app.post_empty(&format!("/api/files/{id}/queue")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // A new download replaces the file while it waits in the queue.
    app.write(
        "Movies/a.mkv",
        &format!("{}{}", h264(), "new content ".repeat(20)),
    );
    fake.set_behavior("a.mkv", Behavior::Skip("Already AV1".into()));
    app.resume().await;
    app.wait_queue_idle().await;
    let file = file_detail(&app, &id).await["file"].clone();
    assert_eq!(file["status"], "skipped", "{file}");
    assert!(file["saved_bytes"].is_null(), "{file}");
    assert!(file["original_size_bytes"].is_null(), "{file}");
    let ov = app.get("/api/overview").await.json;
    assert_eq!(ov["totals"]["saved_bytes"], 0, "{ov}");
    let _ = lib;
}

#[tokio::test]
async fn closing_a_running_row_never_overwrites_a_recorded_result() {
    let (app, _lib, file) = converted_file(Arc::new(FakeToolkit::default())).await;
    let job: Uuid = file["job_id"].as_str().unwrap().parse().unwrap();
    let mut tx = app.state.db.write_tx().await.unwrap();
    assert!(!db::jobs::cancel_running_row(&mut tx, job).await.unwrap());
    tx.commit().await.unwrap();
    let job = app.get(&format!("/api/jobs/{job}")).await.json;
    assert_eq!(job["state"], "done", "{job}");
    let detail = file_detail(&app, &id_of(&file)).await;
    assert_eq!(detail["file"]["status"], "done");

    // A row really left running is closed, and a converted file stays done.
    app.pause().await;
    let queued = app
        .post_empty(&format!("/api/files/{}/queue", id_of(&file)))
        .await
        .json;
    let pool = app.state.db.pool();
    sqlx::query("UPDATE jobs SET state = 'running' WHERE id = ?")
        .bind(id_of(&queued))
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE files SET status = 'processing' WHERE id = ?")
        .bind(id_of(&file))
        .execute(pool)
        .await
        .unwrap();
    let r = app
        .post_empty(&format!("/api/jobs/{}/cancel", id_of(&queued)))
        .await;
    assert_eq!(r.json["state"], "cancelled", "{}", r.text);
    let detail = file_detail(&app, &id_of(&file)).await;
    assert_eq!(detail["file"]["status"], "done", "{detail}");
}

#[tokio::test]
async fn without_cpu_fallback_only_files_that_need_converting_are_refused() {
    let app = TestApp::new().await;
    app.pause().await;
    let r = app
        .patch(
            "/api/settings",
            json!({ "hardware": "nvenc", "cpu_fallback": false }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.write("Movies/new.mkv", h264());
    app.write("Movies/efficient.mkv", av1());
    app.write("Movies/song.flac", audio_only());
    // What the worker does with files the goal leaves as they are.
    app.fake
        .set_behavior("efficient.mkv", Behavior::Skip("Already AV1".into()));
    app.fake.set_behavior(
        "song.flac",
        Behavior::Skip("Audio-only files are left as they are".into()),
    );
    let lib = app.add_library("Movies", json!({})).await;
    let files = app.files_by_name(&id_of(&lib)).await;
    // Queued by hand: the goal leaves these as they are.
    for name in ["efficient.mkv", "song.flac"] {
        let r = app
            .post_empty(&format!("/api/files/{}/queue", id_of(&files[name])))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{name}: {}", r.text);
    }
    app.resume().await;
    app.wait_queue_idle().await;

    let files = app.files_by_name(&id_of(&lib)).await;
    let refused = "You chose NVIDIA NVENC, but no working NVIDIA encoder was found here, and \
                   converting on the CPU instead is turned off";
    assert_eq!(files["new.mkv"]["status"], "failed");
    assert!(
        files["new.mkv"]["error"]
            .as_str()
            .unwrap()
            .starts_with(refused),
        "{}",
        files["new.mkv"]
    );
    assert_eq!(
        files["efficient.mkv"]["status"], "skipped",
        "{}",
        files["efficient.mkv"]
    );
    assert_eq!(files["efficient.mkv"]["skip_reason"], "Already AV1");
    assert_eq!(
        files["song.flac"]["status"], "skipped",
        "{}",
        files["song.flac"]
    );

    // "Convert anyway" needs the encoder: refused like any conversion.
    let id = id_of(&files["efficient.mkv"]);
    let r = app
        .post(&format!("/api/files/{id}/queue"), json!({ "force": true }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.wait_queue_idle().await;
    let file = file_detail(&app, &id).await["file"].clone();
    assert_eq!(file["status"], "failed", "{file}");
    assert!(
        file["error"].as_str().unwrap().starts_with(refused),
        "{file}"
    );
    // Only the files that needed no encoder reached the converter, once.
    let mut handed: Vec<String> = app
        .fake
        .run_specs
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.input.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    handed.sort();
    assert_eq!(handed, ["efficient.mkv", "song.flac"]);
}

/// NVENC encoders whose test found every session taken (`busy`), or that
/// passed.
fn with_nvenc(app: &TestApp, busy: bool) -> chrysopoeia_core::HardwareInfo {
    let mut hw = (*app.state.hardware.current().unwrap()).clone();
    hw.encoders.retain(|e| e.api != HwApi::Nvenc);
    for codec in [VideoCodec::Hevc, VideoCodec::Av1, VideoCodec::H264] {
        let name = match codec {
            VideoCodec::Hevc => "hevc_nvenc",
            VideoCodec::Av1 => "av1_nvenc",
            _ => "h264_nvenc",
        };
        hw.encoders.push(EncoderStatus {
            name: name.into(),
            codec,
            api: HwApi::Nvenc,
            available: true,
            verified: !busy,
            device: None,
            error: busy.then(|| format!("{} (details)", chrysopoeia_hwdetect::NVENC_BUSY_SENTENCE)),
        });
    }
    hw.hints.clear();
    hw
}

async fn detect(app: &TestApp) {
    let r = app.post_empty("/api/hardware/detect").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

#[tokio::test]
async fn jobs_wait_for_a_busy_chosen_gpu_instead_of_failing() {
    let app = TestApp::new().await;
    *app.fake.hardware.lock().unwrap() = Some(with_nvenc(&app, true));
    detect(&app).await;
    app.pause().await;
    let r = app
        .patch(
            "/api/settings",
            json!({ "hardware": "nvenc", "cpu_fallback": false }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let hw = app.get("/api/hardware").await.json;
    let hint = hw["hints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["title"] == "The hardware you chose isn't working")
        .cloned()
        .unwrap_or_else(|| panic!("{hw}"));
    assert_eq!(hint["level"], "warning", "{hint}");
    let detail = hint["detail"].as_str().unwrap();
    assert!(detail.contains("in use by other apps"), "{detail}");
    assert!(
        detail.contains("conversions wait until it is free"),
        "{detail}"
    );

    app.write("Movies/a.mkv", h264());
    app.write("Movies/b.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    app.resume().await;
    wait_until("the library to wait for the GPU", || async {
        app.state.dispatcher.hardware_waiting_count() == 1
            && app.state.dispatcher.running_count() == 0
    })
    .await;
    let files = app.files_by_name(&id_of(&lib)).await;
    for name in ["a.mkv", "b.mkv"] {
        assert_eq!(files[name]["status"], "queued", "{}", files[name]);
    }
    assert!(app.fake.run_specs.lock().unwrap().is_empty());
    let q = app.get("/api/queue").await.json;
    assert_eq!(q["queued"], 2, "{q}");
    assert_eq!(q["running"], 0, "{q}");
    // Said once, not per file.
    let feed = app.get("/api/activity").await.json["items"].clone();
    let waiting: Vec<&Value> = feed
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            e["message"]
                .as_str()
                .unwrap_or("")
                .contains("so conversions that need it wait until it is free")
        })
        .collect();
    assert_eq!(waiting.len(), 1, "{feed}");
    assert_eq!(waiting[0]["level"], "warning");

    // The GPU is free at the next check: the jobs run on it.
    *app.fake.hardware.lock().unwrap() = Some(with_nvenc(&app, false));
    detect(&app).await;
    app.wait_queue_idle().await;
    let files = app.files_by_name(&id_of(&lib)).await;
    for name in ["a.mkv", "b.mkv"] {
        assert_eq!(files[name]["status"], "done", "{}", files[name]);
    }
    let encoders: Vec<String> = app
        .fake
        .run_specs
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.candidates[0].name.clone())
        .collect();
    assert_eq!(encoders, ["av1_nvenc", "av1_nvenc"]);
}

#[tokio::test]
async fn bulk_queue_leaves_out_files_the_size_rule_already_kept() {
    let app = TestApp::new().await;
    const KEPT: &str = "Only 6% smaller — kept the original";
    app.fake.set_behavior("a.mkv", Behavior::Skip(KEPT.into()));
    app.write("Movies/a.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    app.wait_queue_idle().await;
    let files = app.files_by_name(&id_of(&lib)).await;
    assert_eq!(files["a.mkv"]["skip_reason"], KEPT, "{}", files["a.mkv"]);
    // Skipped by hand: the goal would convert it, so it is queued.
    app.pause().await;
    app.write("Movies/b.mkv", h264());
    app.rescan(&id_of(&lib)).await;
    let files = app.files_by_name(&id_of(&lib)).await;
    let b = id_of(&files["b.mkv"]);
    app.post_empty(&format!("/api/files/{b}/skip")).await;

    let ids = [id_of(&files["a.mkv"]), b];
    let r = app
        .post("/api/files/bulk", json!({ "action": "queue", "ids": ids }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json, json!({ "affected": 1, "left_out": 1 }));
    let files = app.files_by_name(&id_of(&lib)).await;
    assert_eq!(files["a.mkv"]["status"], "skipped");
    assert_eq!(files["b.mkv"]["status"], "queued");

    // "Convert anyway" is still there for it.
    let a = id_of(&files["a.mkv"]);
    let r = app
        .post(&format!("/api/files/{a}/queue"), json!({ "force": true }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

#[tokio::test]
async fn copies_the_watcher_is_waiting_for_count_as_settling() {
    let app = TestApp::new().await;
    app.write("Movies/a.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let lib_id = id_of(&lib);
    let root = PathBuf::from(lib["path"].as_str().unwrap());
    let settling = |n: usize| HashMap::from([(root.clone(), n)]);
    let stats = || async {
        let lib = app.get(&format!("/api/libraries/{lib_id}")).await.json;
        let ov = app.get("/api/overview").await.json;
        (
            lib["stats"]["settling"].as_u64().unwrap(),
            ov["totals"]["settling"].as_u64().unwrap(),
        )
    };
    assert_eq!(stats().await, (0, 0));

    crate::services::library::set_watch_settling(&app.state, &settling(2))
        .await
        .unwrap();
    assert_eq!(stats().await, (2, 2));
    // Stored, so a restart rescans the library.
    let stored = db::libraries::with_settling_files(app.state.db.pool())
        .await
        .unwrap();
    assert!(stored.contains(&uuid_of(&lib)), "{stored:?}");

    // Other folders don't count; copies that finished stop counting.
    crate::services::library::set_watch_settling(
        &app.state,
        &HashMap::from([(PathBuf::from("/elsewhere"), 5)]),
    )
    .await
    .unwrap();
    assert_eq!(stats().await, (0, 0));
}
