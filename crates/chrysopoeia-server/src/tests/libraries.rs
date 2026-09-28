//! Health, API errors, libraries and presets.

use axum::http::StatusCode;
use serde_json::json;

use super::support::*;

#[tokio::test]
async fn health_and_json_errors() {
    let app = TestApp::new().await;
    let r = app.get("/api/health").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["ok"], true);
    assert_eq!(r.json["version"], env!("CARGO_PKG_VERSION"));

    let r = app.get("/api/nope").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json["code"], "not_found");
    assert!(r.json["error"].as_str().unwrap().contains("/api/nope"));

    let r = app.delete("/api/health").await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(r.json["code"], "method_not_allowed");

    // Malformed JSON and bad ids use the same shape.
    let r = app
        .request(
            axum::http::Method::POST,
            "/api/libraries",
            Some(json!("not an object")),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.json["code"].is_string());
    let r = app.get("/api/libraries/not-a-uuid").await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json["code"], "invalid_path_param");
}

#[tokio::test]
async fn body_limit_is_one_megabyte() {
    let app = TestApp::new().await;
    let big = "x".repeat(1_100_000);
    let r = app.post("/api/libraries", json!({ "path": big })).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE, "{}", r.text);
    assert_eq!(r.json["code"], "body_too_large");
}

#[tokio::test]
async fn create_library_happy_path() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/a.mkv", h264());
    let lib = app
        .add_library("Movies", json!({ "goal": "balanced" }))
        .await;
    assert_eq!(lib["name"], "Movies");
    assert_eq!(lib["enabled"], true);
    assert_eq!(lib["profile"]["goal"], "balanced");
    assert_eq!(lib["profile"]["video_codec"], "hevc");
    assert_eq!(lib["stats"]["file_count"], 1);
    assert_eq!(lib["scanning"], false);
    assert!(lib["last_scan_at"].is_string());
    assert!(lib["path_error"].is_null());
    let canonical = std::fs::canonicalize(app.media.join("Movies")).unwrap();
    assert_eq!(lib["path"], canonical.to_str().unwrap());

    let list = app.get("/api/libraries").await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.json.as_array().unwrap().len(), 1);

    // Activity mentions the scan in plain words.
    let act = app.get("/api/activity").await;
    let messages: Vec<&str> = act.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["message"].as_str().unwrap())
        .collect();
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("Scanned Movies: 1 file, 1 need converting")),
        "{messages:?}"
    );
}

#[tokio::test]
async fn create_library_defaults_and_explicit_profile() {
    let app = TestApp::new().await;
    std::fs::create_dir_all(app.media.join("TV")).unwrap();
    std::fs::create_dir_all(app.media.join("Other")).unwrap();
    // Default profile comes from settings.default_profile (Save space).
    let r = app
        .post(
            "/api/libraries",
            json!({ "path": app.media.join("TV").to_str().unwrap(), "name": "  Shows  " }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.json["name"], "Shows");
    assert_eq!(r.json["profile"]["goal"], "save_space");

    // An explicit profile is normalized: WebM can't hold HEVC.
    let mut profile = serde_json::to_value(chrysopoeia_core::TranscodeProfile::from_goal(
        chrysopoeia_core::Goal::Custom,
    ))
    .unwrap();
    profile["video_codec"] = json!("hevc");
    profile["container"] = json!("webm");
    profile["audio_codec"] = json!("opus");
    let r = app
        .post(
            "/api/libraries",
            json!({ "path": app.media.join("Other").to_str().unwrap(), "profile": profile, "goal": "compatible" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["profile"]["container"], "mkv");
    assert_eq!(r.json["profile"]["goal"], "custom");
}

#[tokio::test]
async fn create_library_validation() {
    let app = TestApp::new().await;
    std::fs::create_dir_all(app.media.join("Movies/Action")).unwrap();
    app.write("file.mkv", h264());

    let cases = [
        (
            json!({ "path": "relative/dir" }),
            StatusCode::BAD_REQUEST,
            "path_not_absolute",
        ),
        (
            json!({ "path": "" }),
            StatusCode::BAD_REQUEST,
            "path_required",
        ),
        (
            json!({ "path": app.media.join("missing").to_str().unwrap() }),
            StatusCode::BAD_REQUEST,
            "path_not_found",
        ),
        (
            json!({ "path": app.media.join("file.mkv").to_str().unwrap() }),
            StatusCode::BAD_REQUEST,
            "not_a_directory",
        ),
        (
            json!({ "path": "/x", "bogus": 1 }),
            StatusCode::BAD_REQUEST,
            "invalid_request",
        ),
    ];
    for (body, status, code) in cases {
        let r = app.post("/api/libraries", body.clone()).await;
        assert_eq!(r.status, status, "{body} -> {}", r.text);
        assert_eq!(r.json["code"], code, "{body} -> {}", r.text);
        assert!(
            r.json["error"].as_str().unwrap().ends_with('.'),
            "{}",
            r.text
        );
    }

    let movies = app.media.join("Movies");
    let r = app
        .post(
            "/api/libraries",
            json!({ "path": movies.to_str().unwrap() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    // Identical (also with a trailing slash and a `..` detour).
    for p in [
        movies.to_str().unwrap().to_string(),
        format!("{}/", movies.display()),
        format!("{}/Action/..", movies.display()),
    ] {
        let r = app.post("/api/libraries", json!({ "path": p })).await;
        assert_eq!(r.status, StatusCode::CONFLICT, "{p}");
        assert_eq!(r.json["code"], "library_exists", "{p}");
    }
    // Nested inside, and containing.
    let r = app
        .post(
            "/api/libraries",
            json!({ "path": movies.join("Action").to_str().unwrap() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json["code"], "library_overlaps");
    let r = app
        .post(
            "/api/libraries",
            json!({ "path": app.media.to_str().unwrap() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json["code"], "library_overlaps");
}

#[tokio::test]
async fn library_may_not_contain_the_output_folder() {
    let app = TestApp::new().await;
    let out = app.media.join("All/converted");
    std::fs::create_dir_all(&out).unwrap();
    let r = app
        .patch(
            "/api/settings",
            json!({ "output_mode": "folder", "output_folder": out.to_str().unwrap() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = app
        .post(
            "/api/libraries",
            json!({ "path": app.media.join("All").to_str().unwrap() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json["code"], "contains_output_folder");
}

#[tokio::test]
async fn patch_and_delete_library() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/a.mkv", h264());
    app.write("Movies/b.mkv", "video=hevc\naudio=aac\n");
    let lib = app
        .add_library("Movies", json!({ "goal": "balanced" }))
        .await;
    let id = lib["id"].as_str().unwrap().to_string();
    let files = app.files_by_name(&id).await;
    assert_eq!(files["a.mkv"]["status"], "queued");
    assert_eq!(files["b.mkv"]["status"], "skipped");
    assert_eq!(files["b.mkv"]["skip_reason"], "Already HEVC (H.265)");

    // Rename + profile change: b.mkv (HEVC) now needs converting to AV1.
    let r = app
        .patch(
            &format!("/api/libraries/{id}"),
            json!({ "name": "Films", "profile": chrysopoeia_core::TranscodeProfile::from_goal(chrysopoeia_core::Goal::SaveSpace) }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["name"], "Films");
    assert_eq!(r.json["profile"]["video_codec"], "av1");
    let files = app.files_by_name(&id).await;
    assert_eq!(
        files["b.mkv"]["status"], "queued",
        "re-decided with auto-queue"
    );
    assert_eq!(
        files["a.mkv"]["status"], "queued",
        "queued files are left alone"
    );

    // Invalid name.
    let r = app
        .patch(&format!("/api/libraries/{id}"), json!({ "name": "   " }))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json["code"], "invalid_name");

    // Disable: scans are refused with a plain reason.
    let r = app
        .patch(&format!("/api/libraries/{id}"), json!({ "enabled": false }))
        .await;
    assert_eq!(r.json["enabled"], false);
    let r = app.post_empty(&format!("/api/libraries/{id}/scan")).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json["code"], "library_disabled");

    // Delete: rows go, media stays.
    let r = app.delete(&format!("/api/libraries/{id}")).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(app.media.join("Movies/a.mkv").exists());
    let r = app.get(&format!("/api/libraries/{id}")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json["code"], "library_not_found");
    let r = app.get("/api/files").await;
    assert_eq!(r.json["total"], 0);
    let r = app.get("/api/jobs").await;
    assert_eq!(r.json["total"], 0);
    let r = app.delete(&format!("/api/libraries/{id}")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_library_while_a_job_runs() {
    let app = TestApp::new().await;
    app.fake.set_default(Behavior::Hold);
    app.write("Movies/a.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let app_ref = &app;
    wait_until("running", || async {
        app_ref.state.dispatcher.running_count() == 1
    })
    .await;
    let r = app
        .delete(&format!("/api/libraries/{}", lib["id"].as_str().unwrap()))
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        app.state.dispatcher.running_count(),
        0,
        "its job was cancelled"
    );
    assert_eq!(app.get("/api/jobs").await.json["total"], 0);
    assert!(app.media.join("Movies/a.mkv").exists());
    let q = app.get("/api/queue").await;
    assert_eq!(q.json["running"], 0);
}

#[tokio::test]
async fn scan_endpoints() {
    let app = TestApp::new().await;
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap();
    let r = app.post_empty(&format!("/api/libraries/{id}/scan")).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    assert_eq!(r.json["started"], true);
    app.wait_scan(id).await;
    let r = app.post_empty("/api/scan").await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    assert_eq!(r.json["libraries"], 1);
    app.wait_scan(id).await;
    let r = app
        .post_empty(&format!("/api/libraries/{}/scan", uuid::Uuid::new_v4()))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn missing_library_folder_is_reported() {
    let app = TestApp::new().await;
    let lib = app.add_library("Gone", json!({})).await;
    std::fs::remove_dir_all(app.media.join("Gone")).unwrap();
    let r = app
        .get(&format!("/api/libraries/{}", lib["id"].as_str().unwrap()))
        .await;
    let err = r.json["path_error"].as_str().unwrap();
    assert!(err.contains("missing"), "{err}");
}

#[tokio::test]
async fn presets_describe_goals_and_codecs() {
    let app = TestApp::new().await;
    let r = app.get("/api/presets").await;
    assert_eq!(r.status, StatusCode::OK);
    let goals = r.json["goals"].as_array().unwrap();
    assert_eq!(goals.len(), 4);
    assert_eq!(goals[0]["goal"], "save_space");
    assert_eq!(goals[2]["title"], "Plays everywhere");
    assert_eq!(goals[2]["profile"]["container"], "mp4");
    let vc = r.json["video_codecs"].as_array().unwrap();
    assert_eq!(vc.len(), 4);
    let av1 = vc.iter().find(|c| c["codec"] == "av1").unwrap();
    assert_eq!(av1["royalty_free"], true);
    assert_eq!(av1["hw_accelerated"], false);
    assert_eq!(av1["encoders"], json!(["libsvtav1"]));
    // Only audio encoders this ffmpeg has (the fake has Opus and AAC).
    assert_eq!(r.json["audio_codecs"].as_array().unwrap().len(), 3);
    let webm = r.json["containers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["container"] == "webm")
        .unwrap();
    assert_eq!(webm["video"], json!(["av1", "vp9"]));
    assert_eq!(webm["audio"], json!(["copy", "opus"]));
}
