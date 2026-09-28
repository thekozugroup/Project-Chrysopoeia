//! Regression tests for races and failure modes found in review: scans that
//! overlap a finishing job, a busy database, disconnected library folders,
//! goal changes during a scan, files still being copied, and big libraries.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrysopoeia_core::{Goal, TranscodeProfile, VideoCodec};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::support::*;

async fn activity_text(app: &TestApp) -> String {
    app.get("/api/activity?limit=100").await.json["items"].to_string()
}

async fn wait_job_started(app: &TestApp) {
    let fake = Arc::clone(&app.fake);
    wait_until("a job to start", move || {
        let fake = Arc::clone(&fake);
        async move { !fake.started().is_empty() }
    })
    .await;
}

/// Start a rescan whose walk lists the folder while `file`'s job runs, then
/// let the job finish before the scan looks at what it listed.
async fn rescan_while_job_finishes(file: &str) -> (TestApp, String) {
    let app = TestApp::new().await;
    app.fake.set_default(Behavior::Hold);
    app.write(&format!("Movies/{file}"), h264());
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    wait_job_started(&app).await;

    app.fake.walk_gate.close();
    let r = app.post_empty(&format!("/api/libraries/{id}/scan")).await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    app.fake.walk_gate.wait_reached().await;
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;
    app.fake.walk_gate.release();
    app.wait_scan(&id).await;
    (app, id)
}

#[tokio::test]
async fn a_rescan_during_a_conversion_keeps_the_renamed_result() {
    let (app, id) = rescan_while_job_finishes("movie.mp4").await;
    let files = app.files_by_name(&id).await;
    assert_eq!(files.len(), 1, "{files:#?}");
    let f = &files["movie.mkv"];
    assert_eq!(f["status"], "done", "{f:#}");
    assert!(f["saved_bytes"].as_i64().unwrap() > 0, "{f:#}");
    let feed = activity_text(&app).await;
    assert!(!feed.contains("couldn't be read"), "{feed}");
    assert!(!feed.contains("removed"), "{feed}");
}

#[tokio::test]
async fn a_rescan_during_a_conversion_keeps_the_result_in_place() {
    let (app, id) = rescan_while_job_finishes("show.mkv").await;
    let files = app.files_by_name(&id).await;
    let f = &files["show.mkv"];
    assert_eq!(f["status"], "done", "{f:#}");
    assert!(f["saved_bytes"].as_i64().unwrap() > 0, "{f:#}");
    let on_disk = std::fs::metadata(app.media.join("Movies/show.mkv"))
        .unwrap()
        .len();
    assert_eq!(f["size_bytes"], on_disk, "{f:#}");
}

#[tokio::test]
async fn a_file_that_disappears_during_a_scan_is_not_reported_broken() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/stays.mkv", h264());
    let leaves = app.write("Movies/leaves.mkv", h264());
    app.fake.walk_gate.close();
    let dir = app.media.join("Movies");
    let r = app
        .post("/api/libraries", json!({ "path": dir.to_str().unwrap() }))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let id = r.json["id"].as_str().unwrap().to_string();
    app.fake.walk_gate.wait_reached().await;
    std::fs::remove_file(&leaves).unwrap();
    app.fake.walk_gate.release();
    app.wait_scan(&id).await;
    let files = app.files_by_name(&id).await;
    assert_eq!(files.len(), 1, "{files:#?}");
    assert!(files.contains_key("stays.mkv"));
    assert!(!activity_text(&app).await.contains("couldn't be read"));
}

#[tokio::test]
async fn a_job_result_is_recorded_after_the_database_was_busy() {
    let app = TestApp::start(TestOptions {
        configure: Box::new(|c, _| c.db_busy_timeout = Duration::from_millis(200)),
        ..TestOptions::default()
    })
    .await;
    app.fake.set_default(Behavior::Hold);
    app.write("Movies/movie.mp4", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    wait_job_started(&app).await;

    // Another writer holds the database well past the busy timeout while
    // the job finishes (a huge bulk action on a slow disk).
    let mut tx = app.state.db.write_tx().await.unwrap();
    sqlx::query("UPDATE settings SET value = value WHERE key = 'settings'")
        .execute(&mut *tx)
        .await
        .unwrap();
    app.fake.release.add_permits(1);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        app.state.dispatcher.running_count(),
        1,
        "the job keeps its slot until its result is recorded"
    );
    tx.rollback().await.unwrap();

    app.wait_queue_idle().await;
    let files = app.files_by_name(&id).await;
    assert_eq!(files.len(), 1, "{files:#?}");
    assert_eq!(files["movie.mkv"]["status"], "done");
    let jobs = app.get("/api/jobs?state=history").await;
    assert_eq!(jobs.json["items"][0]["state"], "done");
    assert_eq!(app.get("/api/queue").await.json["running"], 0);
}

/// Five queued files in a library whose folder then goes away.
async fn queued_library() -> (TestApp, String) {
    let app = TestApp::new().await;
    app.pause().await;
    for i in 0..5 {
        app.write(&format!("Movies/m{i}.mkv"), h264());
    }
    let lib = app.add_library("Movies", json!({})).await;
    (app, lib["id"].as_str().unwrap().to_string())
}

async fn assert_waiting_for_library(app: &TestApp, id: &str) {
    let app_ref = app;
    wait_until("the library to be reported offline", || async {
        activity_text(app_ref).await.contains("can't be reached")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let files = app.files_by_name(id).await;
    assert!(
        files.values().all(|f| f["status"] == "queued"),
        "{files:#?}"
    );
    assert!(app.fake.started().is_empty());
    assert_eq!(app.get("/api/queue").await.json["queued"], 5);
}

#[tokio::test]
async fn a_missing_library_folder_holds_its_queue_instead_of_failing_it() {
    let (app, id) = queued_library().await;
    let movies = app.media.join("Movies");
    let away = app.media.join("Movies.away");
    std::fs::rename(&movies, &away).unwrap();
    app.resume().await;
    assert_waiting_for_library(&app, &id).await;

    std::fs::rename(&away, &movies).unwrap();
    app.rescan(&id).await;
    app.wait_queue_idle().await;
    assert_eq!(app.get("/api/files?status=done").await.json["total"], 5);
}

#[tokio::test]
async fn an_empty_mount_point_holds_the_queue_and_explains_why() {
    let (app, id) = queued_library().await;
    let movies = app.media.join("Movies");
    let away = app.media.join("Movies.away");
    // An unmounted share leaves its mount point behind, empty.
    std::fs::rename(&movies, &away).unwrap();
    std::fs::create_dir(&movies).unwrap();
    app.resume().await;
    assert_waiting_for_library(&app, &id).await;
    let lib = app.get(&format!("/api/libraries/{id}")).await;
    assert!(
        lib.json["path_error"].as_str().unwrap().contains("empty"),
        "{}",
        lib.json
    );

    std::fs::remove_dir(&movies).unwrap();
    std::fs::rename(&away, &movies).unwrap();
    app.rescan(&id).await;
    app.wait_queue_idle().await;
    assert_eq!(app.get("/api/files?status=done").await.json["total"], 5);
    let lib = app.get(&format!("/api/libraries/{id}")).await;
    assert!(lib.json["path_error"].is_null(), "{}", lib.json);
}

#[tokio::test]
async fn a_file_that_comes_back_is_looked_at_again() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/other.mkv", av1());
    let file = app.write("Movies/back.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    let aside = app.media.join("back.mkv");
    std::fs::rename(&file, &aside).unwrap();
    app.resume().await;
    app.wait_queue_idle().await;
    let files = app.files_by_name(&id).await;
    assert_eq!(files["back.mkv"]["status"], "failed");
    assert!(
        files["back.mkv"]["error"]
            .as_str()
            .unwrap()
            .starts_with("The file is no longer at")
    );

    // Moved back unchanged: the next scan picks it up again.
    std::fs::rename(&aside, &file).unwrap();
    app.rescan(&id).await;
    app.wait_queue_idle().await;
    let files = app.files_by_name(&id).await;
    assert_eq!(files["back.mkv"]["status"], "done", "{files:#?}");
}

#[tokio::test]
async fn a_goal_changed_during_a_scan_applies_to_the_scanned_files() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/already_av1.mkv", av1());
    app.write("Movies/plain_h264.mkv", h264());
    app.fake.walk_gate.close();
    let dir = app.media.join("Movies");
    let r = app
        .post(
            "/api/libraries",
            json!({ "path": dir.to_str().unwrap(), "goal": "save_space" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let id = r.json["id"].as_str().unwrap().to_string();
    app.fake.walk_gate.wait_reached().await;
    let profile = serde_json::to_value(TranscodeProfile::from_goal(Goal::Compatible)).unwrap();
    let r = app
        .patch(
            &format!("/api/libraries/{id}"),
            json!({ "profile": profile }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.fake.walk_gate.release();
    app.wait_scan(&id).await;

    let files = app.files_by_name(&id).await;
    assert_eq!(files["already_av1.mkv"]["status"], "queued", "{files:#?}");
    assert_eq!(files["plain_h264.mkv"]["status"], "skipped", "{files:#?}");
    assert_eq!(
        files["plain_h264.mkv"]["skip_reason"],
        format!("Already {}", VideoCodec::H264.label())
    );
}

fn make_old(path: &std::path::Path) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
}

#[tokio::test]
async fn files_still_being_copied_wait_until_they_settle() {
    let app = TestApp::start(TestOptions {
        configure: Box::new(|c, _| c.settle = Duration::from_secs(2)),
        ..TestOptions::default()
    })
    .await;
    app.pause().await;
    let old = app.write("Movies/old.mkv", h264());
    make_old(&old);
    app.write("Movies/copying.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();

    let files = app.files_by_name(&id).await;
    assert!(files.contains_key("old.mkv"), "{files:#?}");
    assert!(!files.contains_key("copying.mkv"), "{files:#?}");
    assert!(activity_text(&app).await.contains("still being copied"));
    // Picked up by itself once it has settled.
    let app_ref = &app;
    let id_ref = &id;
    wait_until("the copied file to be found", || async {
        app_ref
            .files_by_name(id_ref)
            .await
            .contains_key("copying.mkv")
    })
    .await;

    // A queued file that is written to again right before its turn waits.
    std::fs::write(&old, format!("{}{}", h264(), "x".repeat(100))).unwrap();
    app.resume().await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(
        !app.fake.started().contains(&"old.mkv".to_string()),
        "converted while still being written"
    );
    app.wait_queue_idle().await;
    assert!(app.fake.started().contains(&"old.mkv".to_string()));
    let files = app.files_by_name(&id).await;
    assert_eq!(files["old.mkv"]["status"], "done", "{files:#?}");
}

#[tokio::test]
async fn profile_edits_keep_size_rule_skips_unless_the_size_could_change() {
    let app = TestApp::new().await;
    let reason = "Only 3% smaller — kept the original";
    app.fake
        .set_behavior("small.mkv", Behavior::Skip(reason.into()));
    app.write("Movies/small.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    app.wait_queue_idle().await;
    let files = app.files_by_name(&id).await;
    assert_eq!(files["small.mkv"]["skip_reason"], reason);
    app.pause().await;

    // Subtitle languages don't change how small the result is.
    let mut profile = lib["profile"].clone();
    profile["subtitle_languages"] = json!(["eng"]);
    let r = app
        .patch(
            &format!("/api/libraries/{id}"),
            json!({ "profile": profile }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let files = app.files_by_name(&id).await;
    assert_eq!(files["small.mkv"]["status"], "skipped");
    assert_eq!(files["small.mkv"]["skip_reason"], reason);

    // A smaller quality might: it's tried again.
    profile["quality"] = json!("small");
    let r = app
        .patch(
            &format!("/api/libraries/{id}"),
            json!({ "profile": profile }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let files = app.files_by_name(&id).await;
    assert_eq!(files["small.mkv"]["status"], "queued");
}

#[tokio::test]
async fn jobs_created_together_run_in_the_order_they_were_queued() {
    let app = TestApp::new().await;
    app.pause().await;
    app.patch("/api/settings", json!({ "max_jobs": 1 })).await;
    for n in 0..20 {
        app.write(&format!("Movies/f{n:02}.mkv"), h264());
    }
    app.add_library("Movies", json!({})).await;
    // As if they had all been created in one busy millisecond.
    sqlx::query("UPDATE jobs SET created_at = '2026-01-01T00:00:00.000Z'")
        .execute(app.state.db.pool())
        .await
        .unwrap();
    let created: Vec<String> = sqlx::query_scalar("SELECT file_name FROM jobs ORDER BY rowid")
        .fetch_all(app.state.db.pool())
        .await
        .unwrap();
    let listed: Vec<String> = app.get("/api/jobs?state=queued&limit=100").await.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["file_name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(listed, created);
    app.resume().await;
    app.wait_queue_idle().await;
    assert_eq!(app.fake.started(), created);
}

/// Insert `n` files into a library directly (much faster than scanning).
async fn insert_files(app: &TestApp, library: &str, n: usize, status: &str, reason: Option<&str>) {
    let probe = serde_json::to_string(&fake_probe(h264(), 100).unwrap()).unwrap();
    let mut tx = app.state.db.write_tx().await.unwrap();
    for i in 0..n {
        sqlx::query(
            "INSERT INTO files (id, library_id, path, relative_path, file_name, size_bytes, \
             modified_at, status, probe, video_codec, skip_reason, scanned_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, 100, '2026-01-01T00:00:00.000Z', ?, ?, 'h264', ?, \
             '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(library)
        .bind(format!("/nowhere/{status}/f{i}.mkv"))
        .bind(format!("f{i}.mkv"))
        .bind(format!("f{i}.mkv"))
        .bind(status)
        .bind(&probe)
        .bind(reason)
        .execute(&mut *tx)
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn bulk_actions_and_redecisions_cover_big_libraries() {
    let app = TestApp::new().await;
    app.pause().await;
    let lib = app
        .add_library("Movies", json!({ "goal": "balanced" }))
        .await;
    let id = lib["id"].as_str().unwrap().to_string();

    insert_files(&app, &id, 1_200, "pending", None).await;
    let r = app
        .post(
            "/api/files/bulk",
            json!({ "action": "queue", "library": id }),
        )
        .await;
    assert_eq!(r.json["affected"], 1_200, "{}", r.text);
    assert_eq!(app.get("/api/queue").await.json["queued"], 1_200);
    let r = app
        .post(
            "/api/files/bulk",
            json!({ "action": "skip", "library": id }),
        )
        .await;
    assert_eq!(r.json["affected"], 1_200, "{}", r.text);
    assert_eq!(app.get("/api/queue").await.json["queued"], 0);
    let skipped_total = || async {
        app.get(&format!("/api/files?library={id}&status=skipped&limit=1"))
            .await
            .json["total"]
            .clone()
    };
    assert_eq!(skipped_total().await, 1_200);

    // Re-deciding after a goal change pages through every file.
    insert_files(&app, &id, 1_100, "skipped", Some("Already HEVC")).await;
    let profile = serde_json::to_value(TranscodeProfile::from_goal(Goal::SaveSpace)).unwrap();
    let r = app
        .patch(
            &format!("/api/libraries/{id}"),
            json!({ "profile": profile }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // The H.264 files are converted under the new goal; files skipped by
    // hand stay skipped.
    assert_eq!(app.get("/api/queue").await.json["queued"], 1_100);
    assert_eq!(skipped_total().await, 1_200);
}

#[tokio::test]
async fn old_job_history_is_trimmed_but_each_files_latest_job_is_kept() {
    let app = TestApp::new().await;
    app.write("Movies/a.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    app.wait_queue_idle().await;
    let file = &app.files_by_name(&id).await["a.mkv"];
    let latest = file["job_id"].as_str().unwrap().to_string();
    let file_id = file["id"].as_str().unwrap().to_string();
    let pool = app.state.db.pool();
    sqlx::query("UPDATE jobs SET finished_at = '2020-01-01T00:00:00.000Z' WHERE id = ?")
        .bind(&latest)
        .execute(pool)
        .await
        .unwrap();
    for i in 0..35 {
        let finished = if i < 30 {
            "2020-01-01T00:00:00.000Z".to_string()
        } else {
            crate::db::now_ts()
        };
        sqlx::query(
            "INSERT INTO jobs (id, file_id, library_id, file_name, file_path, state, stage, \
             created_at, finished_at) VALUES (?, ?, ?, 'a.mkv', '/x', 'failed', 'waiting', ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&file_id)
        .bind(&id)
        .bind(&finished)
        .bind(&finished)
        .execute(pool)
        .await
        .unwrap();
    }
    let deleted = crate::db::jobs::trim_history(&app.state.db).await.unwrap();
    assert_eq!(deleted, 30);
    let left: Vec<String> = sqlx::query_scalar("SELECT id FROM jobs")
        .fetch_all(pool)
        .await
        .unwrap();
    assert_eq!(left.len(), 6);
    assert!(left.contains(&latest));
}

async fn send(
    app: &TestApp,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = app
        .router
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn other_websites_cannot_use_the_api() {
    let app = TestApp::new().await;
    let evil = [("host", "tower:8080"), ("origin", "http://evil.example")];

    // A form on another site posting to the API.
    let (status, body) = send(
        &app,
        "POST",
        "/api/queue/stop",
        &[
            evil[0],
            evil[1],
            ("content-type", "application/x-www-form-urlencoded"),
        ],
        "x=1",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden_origin");
    assert_eq!(app.get("/api/queue").await.json["paused"], false);

    // The UI on the server's own address works.
    let (status, _) = send(
        &app,
        "POST",
        "/api/queue/pause",
        &[("host", "tower:8080"), ("origin", "http://tower:8080")],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(app.get("/api/queue").await.json["paused"], true);

    // Bodies must be JSON, so plain forms never parse as a request.
    let (status, body) = send(
        &app,
        "POST",
        "/api/jobs/clear",
        &[("content-type", "text/plain")],
        r#"{"state":"history"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(body["code"], "unsupported_media_type");

    // DNS rebinding: a foreign host name is refused, even for reads.
    let (status, body) = send(
        &app,
        "GET",
        "/api/fs/browse",
        &[("host", "attacker.example:8080")],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "host_not_allowed");

    // A WebSocket opened by another site.
    let (status, _) = send(
        &app,
        "GET",
        "/api/ws",
        &[
            evil[0],
            evil[1],
            ("connection", "upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-version", "13"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
        ],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn allowed_hosts_admit_a_reverse_proxy_domain() {
    let app = TestApp::start(TestOptions {
        configure: Box::new(|c, _| c.allowed_hosts = vec!["media.example.com".into()]),
        ..TestOptions::default()
    })
    .await;
    let (status, _) = send(
        &app,
        "POST",
        "/api/queue/pause",
        &[
            ("host", "media.example.com"),
            ("origin", "https://media.example.com"),
        ],
        "",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
