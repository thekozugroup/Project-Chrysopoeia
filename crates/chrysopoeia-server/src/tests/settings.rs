//! Settings and hardware endpoints.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;
use tokio::sync::Semaphore;

use super::support::*;

#[tokio::test]
async fn settings_get_and_patch() {
    let app = TestApp::new().await;
    let r = app.get("/api/settings").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["auto_queue"], true);
    assert_eq!(r.json["validation"], "standard");
    assert_eq!(r.json["hardware"], "auto");
    assert!(r.json["max_jobs"].is_null());

    let r = app
        .patch(
            "/api/settings",
            json!({ "max_jobs": 4, "onboarded": true, "temp_dir": "  ", "active_hours": { "start": 22, "end": 6 } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["max_jobs"], 4);
    assert_eq!(r.json["onboarded"], true);
    assert!(r.json["temp_dir"].is_null(), "blank means unset");
    assert_eq!(r.json["active_hours"]["start"], 22);
    // Persisted, and other keys untouched.
    let r = app.get("/api/settings").await;
    assert_eq!(r.json["max_jobs"], 4);
    assert_eq!(r.json["auto_queue"], true);
    let q = app.get("/api/queue").await;
    assert_eq!(q.json["max_jobs"], 4);

    // default_profile is replaced whole and normalized.
    let mut profile = serde_json::to_value(chrysopoeia_core::TranscodeProfile::from_goal(
        chrysopoeia_core::Goal::Compatible,
    ))
    .unwrap();
    profile["audio_codec"] = json!("flac");
    let r = app
        .patch("/api/settings", json!({ "default_profile": profile }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["default_profile"]["container"], "mp4");
    assert_eq!(
        r.json["default_profile"]["audio_codec"], "aac",
        "MP4 can't hold FLAC"
    );
}

#[tokio::test]
async fn settings_validation() {
    let app = TestApp::new().await;
    let file = app.write("x.mkv", "x");
    std::fs::create_dir_all(app.media.join("Movies/out")).unwrap();
    app.add_library("Movies", json!({})).await;
    let cases = vec![
        (json!({ "colour": "gold" }), "unknown_setting"),
        (json!({ "max_jobs": 0 }), "invalid_settings"),
        (json!({ "max_jobs": 33 }), "invalid_settings"),
        (json!({ "max_jobs": "many" }), "invalid_settings"),
        (
            json!({ "active_hours": { "start": 24, "end": 3 } }),
            "invalid_settings",
        ),
        (
            json!({ "ignore_patterns": ["[unclosed"] }),
            "invalid_settings",
        ),
        (json!({ "output_mode": "folder" }), "invalid_settings"),
        (
            json!({ "output_mode": "folder", "output_folder": "/nonexistent/out" }),
            "invalid_settings",
        ),
        (
            json!({ "output_mode": "folder", "output_folder": app.media.join("Movies/out").to_str().unwrap() }),
            "invalid_settings",
        ),
        (
            json!({ "temp_dir": "/nonexistent/scratch" }),
            "invalid_settings",
        ),
        (
            json!({ "temp_dir": "relative/scratch" }),
            "invalid_settings",
        ),
        (
            json!({ "temp_dir": file.to_str().unwrap() }),
            "invalid_settings",
        ),
        (json!({ "validation": "paranoid" }), "invalid_settings"),
        (json!([1, 2, 3]), "invalid_settings"),
    ];
    for (body, code) in cases {
        let r = app.patch("/api/settings", body.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body} -> {}", r.text);
        assert_eq!(r.json["code"], code, "{body} -> {}", r.text);
    }
    let r = app
        .patch(
            "/api/settings",
            json!({ "output_mode": "folder", "output_folder": app.media.join("Movies/out").to_str().unwrap() }),
        )
        .await;
    assert!(
        r.json["error"]
            .as_str()
            .unwrap()
            .contains("inside the library Movies"),
        "{}",
        r.text
    );
    // Nothing was saved by the failed attempts.
    let r = app.get("/api/settings").await;
    assert!(r.json["max_jobs"].is_null());
    assert_eq!(r.json["output_mode"], "replace");
}

#[tokio::test]
async fn hardware_placeholder_until_detection_finishes() {
    let fake = Arc::new(FakeToolkit {
        detect_gate: Arc::new(Semaphore::new(0)),
        ..FakeToolkit::default()
    });
    let app = TestApp::start(TestOptions {
        fake: fake.clone(),
        wait_ready: false,
        ..TestOptions::default()
    })
    .await;
    let r = app.get("/api/hardware").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["hints"][0]["title"], "Checking your hardware…");
    assert_eq!(r.json["hints"][0]["level"], "info");

    // Jobs wait for detection.
    app.write("Movies/a.mkv", h264());
    app.add_library("Movies", json!({})).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(fake.started().is_empty());

    fake.detect_gate.add_permits(10);
    app.wait_ready().await;
    let r = app.get("/api/hardware").await;
    assert_eq!(r.json["cpu"]["model"], "Fake CPU");
    assert_eq!(r.json["recommended_jobs"]["total"], 2);
    app.wait_queue_idle().await;
    assert_eq!(fake.started(), ["a.mkv"]);

    let r = app.post_empty("/api/hardware/detect").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["ffmpeg"]["found"], true);
}
