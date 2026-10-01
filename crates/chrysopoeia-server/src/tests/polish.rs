//! Round 3 polish: "Convert anyway", re-queued finished files, bulk queue
//! filtering, savings per library, overview buckets, files still being
//! copied, the chosen hardware, and prompt cancels.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::support::*;

fn id_of(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn convert_anyway_is_stored_on_the_job_and_passed_to_the_worker() {
    let app = TestApp::new().await;
    app.write("Movies/efficient.mkv", av1());
    let lib = app.add_library("Movies", json!({})).await;
    let files = app.files_by_name(&id_of(&lib)).await;
    let file = &files["efficient.mkv"];
    assert_eq!(file["status"], "skipped", "{file}");
    let id = id_of(file);

    let r = app
        .post(&format!("/api/files/{id}/queue"), json!({ "force": true }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["force"], true);
    app.wait_queue_idle().await;
    let forced: Vec<bool> = app
        .fake
        .run_specs
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.input.ends_with("efficient.mkv"))
        .map(|s| s.force)
        .collect();
    assert_eq!(forced, [true]);
    let detail = app.get(&format!("/api/files/{id}")).await;
    assert_eq!(detail.json["file"]["status"], "done");
    assert_eq!(detail.json["jobs"][0]["force"], true);

    // A plain queue is not forced.
    let r = app.post_empty(&format!("/api/files/{id}/queue")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["force"], false);
    app.wait_queue_idle().await;
    let r = app
        .post(&format!("/api/files/{id}/queue"), json!({ "force": "yes" }))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
}

#[tokio::test]
async fn a_requeued_done_file_whose_job_is_skipped_stays_done() {
    let app = TestApp::new().await;
    app.write("Movies/a.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    app.wait_queue_idle().await;
    let before = app.files_by_name(&id_of(&lib)).await["a.mkv"].clone();
    assert_eq!(before["status"], "done", "{before}");
    let saved = before["saved_bytes"].as_i64().unwrap();
    assert!(saved > 0);

    app.fake
        .set_behavior("a.mkv", Behavior::Skip("Already AV1".into()));
    let id = id_of(&before);
    let r = app.post_empty(&format!("/api/files/{id}/queue")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.wait_queue_idle().await;

    let detail = app.get(&format!("/api/files/{id}")).await;
    let file = &detail.json["file"];
    assert_eq!(file["status"], "done", "{file}");
    assert_eq!(file["saved_bytes"], saved);
    assert!(file["skip_reason"].is_null());
    // The job row records the skip.
    let job = &detail.json["jobs"][0];
    assert_eq!(job["state"], "skipped");
    assert_eq!(job["skip_reason"], "Already AV1");
    let feed = app.get("/api/activity").await.json["items"].to_string();
    assert!(feed.contains("Kept a.mkv as it is: Already AV1"), "{feed}");
    let ov = app.get("/api/overview").await;
    assert_eq!(ov.json["totals"]["done"], 1);
    assert_eq!(ov.json["totals"]["saved_bytes"], saved);
}

#[tokio::test]
async fn bulk_queue_with_ids_leaves_out_files_the_goal_keeps() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/new.mkv", h264());
    app.write("Movies/efficient.mkv", av1());
    app.write("Movies/bad.mkv", "broken");
    app.write("Movies/later.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let files = app.files_by_name(&id_of(&lib)).await;
    // Skipped by hand: the goal would convert it, so it is queued again.
    let later = id_of(&files["later.mkv"]);
    let r = app.post_empty(&format!("/api/files/{later}/skip")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);

    let ids: Vec<String> = ["new.mkv", "efficient.mkv", "bad.mkv", "later.mkv"]
        .iter()
        .map(|n| id_of(&files[*n]))
        .collect();
    let r = app
        .post("/api/files/bulk", json!({ "action": "queue", "ids": ids }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // bad.mkv (failed) and later.mkv are queued; efficient.mkv is already
    // what the goal makes; new.mkv was queued already.
    assert_eq!(r.json, json!({ "affected": 2, "left_out": 1 }));
    let files = app.files_by_name(&id_of(&lib)).await;
    assert_eq!(files["efficient.mkv"]["status"], "skipped");
    assert_eq!(files["bad.mkv"]["status"], "queued");
    assert_eq!(files["later.mkv"]["status"], "queued");

    // Selections by filter report nothing left out.
    let r = app
        .post(
            "/api/files/bulk",
            json!({ "action": "skip", "library": id_of(&lib) }),
        )
        .await;
    assert_eq!(r.json["left_out"], 0);
}

fn history_total(ov: &Value) -> i64 {
    ov["savings_history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["saved_bytes"].as_i64().unwrap())
        .sum()
}

#[tokio::test]
async fn savings_history_follows_the_libraries_in_the_total() {
    let app = TestApp::new().await;
    app.write("Movies/a.mkv", h264());
    app.write("Shows/b.mkv", &format!("{}{}", h264(), "x".repeat(400)));
    let movies = app.add_library("Movies", json!({})).await;
    let shows = app.add_library("Shows", json!({})).await;
    app.wait_queue_idle().await;

    let ov = app.get("/api/overview").await.json;
    let total = ov["totals"]["saved_bytes"].as_i64().unwrap();
    assert!(total > 0);
    assert_eq!(history_total(&ov), total);

    let r = app
        .delete(&format!("/api/libraries/{}", id_of(&shows)))
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.text);
    let ov = app.get("/api/overview").await.json;
    let remaining = ov["totals"]["saved_bytes"].as_i64().unwrap();
    assert!(remaining > 0 && remaining < total, "{remaining} of {total}");
    assert_eq!(history_total(&ov), remaining);
    assert_eq!(ov["totals"]["file_count"], 1);
    let _ = movies;
}

#[tokio::test]
async fn files_without_video_count_as_no_video() {
    let app = TestApp::new().await;
    app.write("Mixed/song.flac", audio_only());
    app.write("Mixed/film.mkv", h264());
    app.add_library("Mixed", json!({})).await;
    let ov = app.get("/api/overview").await.json;
    let names: Vec<(&str, u64)> = ov["resolutions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| (b["name"].as_str().unwrap(), b["files"].as_u64().unwrap()))
        .collect();
    assert!(names.contains(&("No video", 1)), "{names:?}");
    assert!(!names.iter().any(|(n, _)| *n == "Unknown"), "{names:?}");
}

#[tokio::test]
async fn files_still_being_copied_are_counted_until_they_settle() {
    let app = TestApp::start(TestOptions {
        configure: Box::new(|c, _| c.settle = Duration::from_secs(2)),
        ..TestOptions::default()
    })
    .await;
    app.pause().await;
    app.write("Movies/copying.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    assert_eq!(lib["stats"]["settling"], 1, "{lib}");
    assert_eq!(lib["stats"]["file_count"], 0);
    let ov = app.get("/api/overview").await.json;
    assert_eq!(ov["totals"]["settling"], 1);

    let id = id_of(&lib);
    wait_until_for("the file to settle", Duration::from_secs(15), || async {
        let lib = app.get(&format!("/api/libraries/{id}")).await.json;
        lib["stats"]["settling"] == 0 && lib["stats"]["file_count"] == 1
    })
    .await;
    assert_eq!(app.get("/api/overview").await.json["totals"]["settling"], 0);
}

#[tokio::test]
async fn a_chosen_api_without_a_working_encoder_is_explained_and_respected() {
    const HINT: &str = "The hardware you chose isn't working";
    let app = TestApp::new().await;
    app.pause().await;
    let r = app
        .patch("/api/settings", json!({ "hardware": "nvenc" }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let hw = app.get("/api/hardware").await.json;
    let hint = hw["hints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["title"] == HINT)
        .cloned()
        .unwrap_or_else(|| panic!("no hint: {hw}"));
    assert_eq!(hint["level"], "warning");
    assert!(
        hint["detail"].as_str().unwrap().starts_with(
            "You chose NVIDIA NVENC, but no working NVIDIA encoder was found here, so files are \
             converted on the CPU."
        ),
        "{hint}"
    );

    // Converted on the CPU, and the job says why.
    app.write("Movies/a.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    app.resume().await;
    app.wait_queue_idle().await;
    let files = app.files_by_name(&id_of(&lib)).await;
    assert_eq!(files["a.mkv"]["status"], "done");
    let job = app
        .get(&format!(
            "/api/jobs/{}",
            files["a.mkv"]["job_id"].as_str().unwrap()
        ))
        .await
        .json;
    assert!(
        job["notes"].as_array().unwrap().iter().any(|n| n
            == "You chose NVIDIA NVENC, but no working NVIDIA encoder was found here, so this \
                file was converted on the CPU"),
        "{job}"
    );

    // Without CPU fallback, nothing is converted on the CPU behind the
    // user's back.
    let r = app
        .patch("/api/settings", json!({ "cpu_fallback": false }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let hw = app.get("/api/hardware").await.json;
    let hint = hw["hints"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["title"] == HINT)
        .cloned()
        .unwrap();
    assert_eq!(hint["level"], "error");
    app.write("Movies/b.mkv", h264());
    app.rescan(&id_of(&lib)).await;
    app.wait_queue_idle().await;
    let files = app.files_by_name(&id_of(&lib)).await;
    let b = &files["b.mkv"];
    assert_eq!(b["status"], "failed", "{b}");
    assert!(
        b["error"].as_str().unwrap().starts_with(
            "You chose NVIDIA NVENC, but no working NVIDIA encoder was found here, and \
             converting on the CPU instead is turned off"
        ),
        "{b}"
    );
    assert!(
        !app.fake
            .run_specs
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.input.ends_with("b.mkv")),
        "the file was not handed to the converter"
    );

    // Automatic again: the hint goes.
    let r = app
        .patch("/api/settings", json!({ "hardware": "auto" }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let hw = app.get("/api/hardware").await.json;
    assert!(
        !hw["hints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["title"] == HINT),
        "{hw}"
    );
}

#[tokio::test]
async fn cancelling_a_running_job_answers_at_once_with_the_job_stopped() {
    let fake = Arc::new(FakeToolkit::default());
    fake.set_default(Behavior::Hold);
    let app = TestApp::start(TestOptions {
        fake,
        ..TestOptions::default()
    })
    .await;
    app.write("Movies/a.mkv", h264());
    app.add_library("Movies", json!({})).await;
    wait_until("the job to run", || async {
        app.get("/api/jobs?state=running").await.json["total"] == 1
    })
    .await;
    let job = app.get("/api/jobs?state=running").await.json["items"][0].clone();

    let started = Instant::now();
    let r = app
        .post_empty(&format!("/api/jobs/{}/cancel", id_of(&job)))
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["state"], "cancelled");
    let q = app.get("/api/queue").await.json;
    assert_eq!(q["running"], 0, "{q}");
}
