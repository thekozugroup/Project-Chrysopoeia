//! Contract additions and follow-ups from integration: `/api/system`, error
//! fields, job notes, the presets filter, the CPU job cap and busy GPUs.

use std::sync::atomic::Ordering;
use std::time::Duration;

use axum::http::StatusCode;
use serde_json::json;
use szalinski_core::{
    EncoderStatus, HardwareInfo, HwApi, JobRecommendation, SetupHint, SetupHintLevel, VideoCodec,
};

use super::support::*;
use crate::services::hardware::keep_busy_verifications;

/// The fake machine with a working NVIDIA GPU for HEVC (not AV1): three GPU
/// jobs, one CPU job.
fn with_nvenc(base: &HardwareInfo) -> HardwareInfo {
    let mut hw = base.clone();
    hw.encoders.push(EncoderStatus {
        name: "hevc_nvenc".into(),
        codec: VideoCodec::Hevc,
        api: HwApi::Nvenc,
        available: true,
        verified: true,
        device: None,
        error: None,
    });
    hw.recommended_jobs = JobRecommendation {
        cpu_jobs: 1,
        gpu_jobs: 3,
        total: 3,
        reason: "fake GPU".into(),
    };
    hw
}

async fn use_hardware(app: &TestApp, hw: HardwareInfo) {
    *app.fake.hardware.lock().unwrap() = Some(hw);
    let r = app.post_empty("/api/hardware/detect").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

fn base_hardware(app: &TestApp) -> HardwareInfo {
    (*app.state.hardware.current().unwrap()).clone()
}

#[tokio::test]
async fn system_info_names_the_work_folder() {
    let app = TestApp::start(TestOptions {
        configure: Box::new(|c, root| c.temp_dir = Some(root.join("scratch"))),
        ..TestOptions::default()
    })
    .await;
    let r = app.get("/api/system").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["version"], env!("CARGO_PKG_VERSION"));
    let scratch = app.dir.path().join("scratch");
    assert_eq!(r.json["default_temp_dir"], scratch.to_str().unwrap());
    assert_eq!(r.json["browse_roots"][0], app.media.to_str().unwrap());
    assert!(r.json["data_dir"].as_str().unwrap().ends_with("/data"));
    assert!(r.json["in_container"].is_boolean());
}

#[tokio::test]
async fn validation_errors_name_their_field() {
    let app = TestApp::new().await;
    let missing = app.dir.path().join("nope");
    let r = app
        .patch(
            "/api/settings",
            json!({ "temp_dir": missing.to_str().unwrap() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json["code"], "invalid_settings");
    assert_eq!(r.json["field"], "temp_dir");

    let r = app.patch("/api/settings", json!({ "max_jobs": 99 })).await;
    assert_eq!(r.json["field"], "max_jobs");
    let r = app
        .patch("/api/settings", json!({ "ignore_patterns": ["!Movies"] }))
        .await;
    assert_eq!(r.json["field"], "ignore_patterns");
    assert!(
        r.json["error"]
            .as_str()
            .unwrap()
            .contains("exceptions are not supported"),
        "{}",
        r.text
    );
    let r = app
        .patch("/api/settings", json!({ "max_jobs": "lots" }))
        .await;
    assert_eq!(r.json["field"], "max_jobs");
    let r = app
        .patch("/api/settings", json!({ "colour": "gold" }))
        .await;
    assert_eq!(r.json["code"], "unknown_setting");
    assert_eq!(r.json["field"], "colour");
    // A valid change has no error at all.
    let r = app.patch("/api/settings", json!({ "max_jobs": 2 })).await;
    assert_eq!(r.status, StatusCode::OK);

    // Libraries: the path, the name and the profile.
    let r = app
        .post(
            "/api/libraries",
            json!({ "path": missing.to_str().unwrap() }),
        )
        .await;
    assert_eq!(r.json["code"], "path_not_found");
    assert_eq!(r.json["field"], "path");
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap();
    let r = app
        .patch(&format!("/api/libraries/{id}"), json!({ "name": "  " }))
        .await;
    assert_eq!(r.json["field"], "name");
    let mut profile = lib["profile"].clone();
    profile["max_height"] = json!(10);
    let r = app
        .patch(
            &format!("/api/libraries/{id}"),
            json!({ "profile": profile }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json["field"], "profile.max_height");
    let mut profile = lib["profile"].clone();
    profile["quality"] = json!("glorious");
    let r = app
        .patch(
            &format!("/api/libraries/{id}"),
            json!({ "profile": profile }),
        )
        .await;
    assert_eq!(r.json["code"], "invalid_request");
    assert_eq!(r.json["field"], "profile.quality", "{}", r.text);
}

#[tokio::test]
async fn job_notes_are_stored_and_served() {
    let app = TestApp::new().await;
    *app.fake.done_notes.lock().unwrap() =
        vec!["Removed 2 picture-based subtitles because MP4 can't hold them".into()];
    app.write("Movies/a.mkv", h264());
    app.add_library("Movies", json!({})).await;
    app.wait_queue_idle().await;
    let r = app.get("/api/jobs?state=history").await;
    let job = &r.json["items"][0];
    assert_eq!(job["state"], "done", "{}", r.text);
    assert_eq!(
        job["notes"],
        json!(["Removed 2 picture-based subtitles because MP4 can't hold them"])
    );
    let id = job["id"].as_str().unwrap();
    let r = app.get(&format!("/api/jobs/{id}")).await;
    assert_eq!(r.json["notes"].as_array().unwrap().len(), 1);
    // Jobs without notes have an empty list, never null.
    *app.fake.done_notes.lock().unwrap() = Vec::new();
    app.write("Movies/b.mkv", h264());
    let lib = app.get("/api/libraries").await.json[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    app.rescan(&lib).await;
    app.wait_queue_idle().await;
    let r = app.get("/api/jobs?state=history").await;
    let b = r.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["file_name"] == "b.mkv")
        .unwrap()
        .clone();
    assert_eq!(b["notes"], json!([]));
}

#[tokio::test]
async fn presets_offer_only_codecs_this_machine_can_write() {
    let app = TestApp::new().await;
    let r = app.get("/api/presets").await;
    let audio: Vec<&str> = r.json["audio_codecs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["codec"].as_str().unwrap())
        .collect();
    // The fake ffmpeg has only the Opus and AAC encoders.
    assert_eq!(audio, ["copy", "opus", "aac"]);
    let mkv = r.json["containers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["container"] == "mkv")
        .unwrap()
        .clone();
    assert_eq!(mkv["audio"], json!(["copy", "opus", "aac"]));

    // An encoder missing from the ffmpeg build (listed, not available)
    // takes its codec out of the offer.
    let mut hw = base_hardware(&app);
    for e in &mut hw.encoders {
        if e.name == "libsvtav1" {
            e.available = false;
            e.verified = false;
            e.error = Some("This ffmpeg build doesn't include it.".into());
        }
    }
    use_hardware(&app, hw).await;
    let r = app.get("/api/presets").await;
    let video: Vec<&str> = r.json["video_codecs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["codec"].as_str().unwrap())
        .collect();
    assert_eq!(video, ["hevc", "h264", "vp9"]);
    let mkv = r.json["containers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["container"] == "mkv")
        .unwrap()
        .clone();
    assert!(!mkv["video"].as_array().unwrap().contains(&json!("av1")));
}

/// HW-5: with the automatic job count following the GPU (3), jobs that
/// encode on the CPU still run only as many at once as the CPU can take (1).
#[tokio::test]
async fn cpu_encodes_are_capped_when_the_limit_follows_the_gpu() {
    let app = TestApp::new().await;
    app.pause().await;
    let hw = with_nvenc(&base_hardware(&app));
    use_hardware(&app, hw).await;
    app.fake.set_default(Behavior::Hold);
    for n in 0..3 {
        app.write(&format!("Anime/av1-{n}.mkv"), h264());
        app.write(&format!("Shows/hevc-{n}.mkv"), h264());
    }
    // Save space (AV1: no hardware encoder) and Balanced (HEVC on NVENC).
    app.add_library("Anime", json!({ "goal": "save_space" }))
        .await;
    app.add_library("Shows", json!({ "goal": "balanced" }))
        .await;
    let q = app.get("/api/queue").await;
    assert_eq!(q.json["max_jobs"], 3);
    assert_eq!(q.json["max_jobs_auto"], true);
    app.resume().await;
    let app_ref = &app;
    wait_until("3 running", || async {
        app_ref.fake.running.load(Ordering::SeqCst) == 3
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = app.fake.started();
    let cpu = started.iter().filter(|n| n.starts_with("av1-")).count();
    assert_eq!(cpu, 1, "{started:?}");
    assert_eq!(app.state.dispatcher.software_running_count(), 1);
    // Let everything finish: the AV1 jobs take turns.
    app.fake.release.add_permits(6);
    app.wait_queue_idle().await;
    assert_eq!(app.fake.started().len(), 6);

    // A number chosen in Settings is the user's call: no cap.
    for n in 3..6 {
        app.write(&format!("Anime/av1-{n}.mkv"), h264());
    }
    app.patch("/api/settings", json!({ "max_jobs": 3 })).await;
    let libs = app.get("/api/libraries").await.json;
    let lib = libs
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "Anime")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    app.rescan(&lib).await;
    wait_until("3 CPU jobs", || async {
        app_ref.state.dispatcher.software_running_count() == 3
    })
    .await;
    app.fake.release.add_permits(3);
    app.wait_queue_idle().await;
}

/// HW-4: a check made while every NVENC session is taken (e.g. by our own
/// jobs) keeps the earlier successful test instead of turning NVENC off.
#[tokio::test]
async fn a_busy_gpu_keeps_its_earlier_verification() {
    let app = TestApp::new().await;
    let good = with_nvenc(&base_hardware(&app));
    use_hardware(&app, good.clone()).await;
    let mut busy = good.clone();
    for e in &mut busy.encoders {
        if e.name == "hevc_nvenc" {
            e.verified = false;
            e.error = Some(format!(
                "{} Details: OpenEncodeSessionEx failed: incompatible client key (21)",
                szalinski_hwdetect::NVENC_BUSY_SENTENCE
            ));
        }
    }
    busy.recommended_jobs = JobRecommendation {
        cpu_jobs: 1,
        gpu_jobs: 0,
        total: 1,
        reason: "no GPU".into(),
    };
    busy.hints = vec![SetupHint {
        level: SetupHintLevel::Warning,
        title: szalinski_hwdetect::NVIDIA_BUSY_TITLE.into(),
        detail: "busy".into(),
        fix: None,
    }];
    use_hardware(&app, busy.clone()).await;
    let r = app.get("/api/hardware").await;
    let nvenc = r.json["encoders"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "hevc_nvenc")
        .unwrap()
        .clone();
    assert_eq!(nvenc["verified"], true, "{}", r.text);
    assert!(r.json["hints"].as_array().unwrap().is_empty());
    // The fake recommends whatever the hardware says: the kept result
    // restores the GPU's job count on a real machine; here it stays.
    assert_eq!(r.json["detecting"], false);

    // Pure function: no earlier success, nothing to keep.
    let mut fresh = busy.clone();
    let kept = keep_busy_verifications(&mut fresh, &busy);
    assert!(kept.is_empty());
    assert_eq!(fresh.hints.len(), 1);
    let kept = keep_busy_verifications(&mut fresh, &good);
    assert_eq!(kept, ["hevc_nvenc"]);
    assert!(fresh.hints.is_empty());
}

/// A renamed file or folder keeps its state (a finished conversion, a skip)
/// instead of being probed and converted again, both when a scan finds the
/// rename and when the folder watcher reports it.
#[tokio::test]
async fn renamed_files_keep_their_state() {
    let app = TestApp::new().await;
    app.write("Shows/Season 1/a.mkv", h264());
    app.write("Shows/Season 1/b.mkv", h264());
    app.fake.set_behavior(
        "b.mkv",
        Behavior::Skip("Only 3% smaller — kept the original".into()),
    );
    let lib = app
        .add_library("Shows", json!({ "goal": "balanced" }))
        .await;
    let id = lib["id"].as_str().unwrap().to_string();
    app.wait_queue_idle().await;
    let before = app.files_by_name(&id).await;
    assert_eq!(before["a.mkv"]["status"], "done");
    assert_eq!(before["b.mkv"]["status"], "skipped");
    let started = app.fake.started().len();
    let probes = app.fake.probes.load(Ordering::SeqCst);

    // Found by a scan: the folder was renamed while nobody watched.
    let root = std::fs::canonicalize(app.media.join("Shows")).unwrap();
    std::fs::rename(root.join("Season 1"), root.join("Season One")).unwrap();
    app.rescan(&id).await;
    let after = app.files_by_name(&id).await;
    assert_eq!(after.len(), 2);
    assert_eq!(after["a.mkv"]["relative_path"], "Season One/a.mkv");
    assert_eq!(after["a.mkv"]["status"], "done");
    assert_eq!(
        after["a.mkv"]["saved_bytes"],
        before["a.mkv"]["saved_bytes"]
    );
    assert_eq!(after["b.mkv"]["status"], "skipped");
    assert_eq!(
        after["b.mkv"]["skip_reason"],
        "Only 3% smaller — kept the original"
    );
    assert_eq!(
        app.fake.probes.load(Ordering::SeqCst),
        probes,
        "moved files were probed"
    );
    let feed = app.get("/api/activity").await.text;
    assert!(feed.contains("2 moved or renamed"), "{feed}");

    // Reported by the watcher: removal first, the new name after settling.
    let tx = app.fake.watch_sender();
    std::fs::rename(
        root.join("Season One/a.mkv"),
        root.join("Season One/A (2020).mkv"),
    )
    .unwrap();
    tx.send(szalinski_scanner::WatchEvent::Removed(
        root.join("Season One/a.mkv"),
    ))
    .await
    .unwrap();
    tx.send(szalinski_scanner::WatchEvent::Upserted(
        root.join("Season One/A (2020).mkv"),
    ))
    .await
    .unwrap();
    let app_ref = &app;
    let id_c = id.clone();
    wait_until("renamed file stored", || async {
        app_ref
            .files_by_name(&id_c)
            .await
            .contains_key("A (2020).mkv")
    })
    .await;
    let after = app.files_by_name(&id).await;
    assert!(!after.contains_key("a.mkv"));
    assert_eq!(after["A (2020).mkv"]["status"], "done");
    assert_eq!(app.fake.probes.load(Ordering::SeqCst), probes);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        app.fake.started().len(),
        started,
        "a moved file was converted again"
    );
}
