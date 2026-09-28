//! `/api/files`, `/api/jobs` and `/api/queue`.

use axum::http::StatusCode;
use serde_json::json;

use super::support::*;

/// A paused app with a library of five files: three need converting.
async fn fixture() -> (TestApp, String) {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/Alpha.mkv", h264());
    app.write("Movies/beta.mkv", &format!("{}{}", h264(), "x".repeat(200)));
    app.write("Movies/Gamma 50%_off.mkv", h264());
    app.write("Movies/done.mkv", av1());
    app.write("Movies/bad.mkv", "broken");
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    (app, id)
}

fn names(r: &Resp) -> Vec<String> {
    r.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["file_name"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn list_filters_sort_search_and_paginate() {
    let (app, lib) = fixture().await;
    let r = app.get("/api/files").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["total"], 5);
    assert_eq!(
        names(&r),
        [
            "Alpha.mkv",
            "bad.mkv",
            "beta.mkv",
            "done.mkv",
            "Gamma 50%_off.mkv"
        ],
        "name sort is case-insensitive"
    );

    let r = app.get("/api/files?sort=-size&limit=1").await;
    assert_eq!(names(&r), ["beta.mkv"]);
    assert_eq!(r.json["total"], 5);
    let r = app.get("/api/files?sort=name&limit=2&offset=2").await;
    assert_eq!(names(&r), ["beta.mkv", "done.mkv"]);

    let r = app.get("/api/files?status=queued").await;
    assert_eq!(r.json["total"], 3);
    let r = app.get("/api/files?status=failed,skipped").await;
    assert_eq!(names(&r), ["bad.mkv", "done.mkv"]);
    let r = app
        .get(&format!("/api/files?library={lib}&status=skipped"))
        .await;
    assert_eq!(r.json["total"], 1);
    let r = app
        .get(&format!("/api/files?library={}", uuid::Uuid::new_v4()))
        .await;
    assert_eq!(r.json["total"], 0);

    // Search matches name or path, case-insensitively; % and _ are literal.
    let r = app.get("/api/files?q=ALPHA").await;
    assert_eq!(names(&r), ["Alpha.mkv"]);
    let r = app.get("/api/files?q=50%25_").await;
    assert_eq!(names(&r), ["Gamma 50%_off.mkv"]);
    let r = app.get("/api/files?q=%25").await;
    assert_eq!(names(&r), ["Gamma 50%_off.mkv"]);

    let r = app.get("/api/files?sort=status").await;
    assert_eq!(names(&r)[4], "done.mkv", "skipped sorts last");

    for (q, code) in [
        ("status=nope", "invalid_status"),
        ("sort=colour", "invalid_sort"),
        ("library=xyz", "invalid_library"),
        ("limit=abc", "invalid_query"),
    ] {
        let r = app.get(&format!("/api/files?{q}")).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(r.json["code"], code, "{q}");
    }
    let r = app.get("/api/files?limit=100000").await;
    assert_eq!(r.status, StatusCode::OK, "limits are clamped");
}

#[tokio::test]
async fn file_detail_queue_and_skip() {
    let (app, _lib) = fixture().await;
    let files = app.get("/api/files?q=done").await;
    let done_id = files.json["items"][0]["id"].as_str().unwrap().to_string();

    let r = app.get(&format!("/api/files/{done_id}")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["file"]["probe"]["streams"][0]["codec"], "av1");
    assert_eq!(r.json["jobs"], json!([]));

    // Queue a skipped file (re-encode), then again: conflict.
    let r = app
        .post(
            &format!("/api/files/{done_id}/queue"),
            json!({ "priority": 5 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["state"], "queued");
    assert_eq!(r.json["priority"], 5);
    assert_eq!(r.json["file_name"], "done.mkv");
    let job_id = r.json["id"].as_str().unwrap().to_string();
    let r = app.post_empty(&format!("/api/files/{done_id}/queue")).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json["code"], "already_queued");

    let r = app.get(&format!("/api/files/{done_id}")).await;
    assert_eq!(r.json["file"]["status"], "queued");
    assert_eq!(r.json["file"]["job_id"], job_id.as_str());
    assert_eq!(r.json["jobs"].as_array().unwrap().len(), 1);

    // Skip it: the queued job is cancelled.
    let r = app.post_empty(&format!("/api/files/{done_id}/skip")).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["status"], "skipped");
    assert_eq!(r.json["skip_reason"], "Skipped by you");
    let r = app.get(&format!("/api/jobs/{job_id}")).await;
    assert_eq!(r.json["state"], "cancelled");

    let r = app
        .get(&format!("/api/files/{}", uuid::Uuid::new_v4()))
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json["code"], "file_not_found");
}

#[tokio::test]
async fn bulk_actions() {
    let (app, lib) = fixture().await;
    // Skip everything pending/queued (the default selection).
    let r = app
        .post("/api/files/bulk", json!({ "action": "skip" }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["affected"], 3);
    let r = app.get("/api/files?status=skipped").await;
    assert_eq!(r.json["total"], 4);
    let r = app.get("/api/jobs?state=queued").await;
    assert_eq!(r.json["total"], 0);

    // A bare "queue" only takes pending files: none left.
    let r = app
        .post("/api/files/bulk", json!({ "action": "queue" }))
        .await;
    assert_eq!(r.json["affected"], 0);
    // Explicit ids are honoured.
    let alpha = app.get("/api/files?q=Alpha").await.json["items"][0]["id"].clone();
    let r = app
        .post(
            "/api/files/bulk",
            json!({ "action": "queue", "ids": [alpha] }),
        )
        .await;
    assert_eq!(r.json["affected"], 1);
    // Status filter + library.
    let r = app
        .post(
            "/api/files/bulk",
            json!({ "action": "queue", "library": lib, "status": "skipped" }),
        )
        .await;
    assert_eq!(r.json["affected"], 3);
    // Retry failed.
    let r = app
        .post("/api/files/bulk", json!({ "action": "retry_failed" }))
        .await;
    assert_eq!(r.json["affected"], 1);
    let r = app.get("/api/jobs?state=queued").await;
    assert_eq!(r.json["total"], 5);

    let r = app
        .post("/api/files/bulk", json!({ "action": "explode" }))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .post(
            "/api/files/bulk",
            json!({ "action": "queue", "status": ["nope"] }),
        )
        .await;
    assert_eq!(r.json["code"], "invalid_status");
}

#[tokio::test]
async fn jobs_cancel_priority_and_clear() {
    let (app, _lib) = fixture().await;
    let r = app.get("/api/jobs?state=queued").await;
    assert_eq!(r.json["total"], 3);
    let ids: Vec<String> = r.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["id"].as_str().unwrap().to_string())
        .collect();

    // Move the last one to the top.
    let r = app
        .post(
            &format!("/api/jobs/{}/priority", ids[2]),
            json!({ "move": "top" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["priority"], 1);
    let r = app.get("/api/jobs?state=queued").await;
    assert_eq!(r.json["items"][0]["id"], ids[2].as_str());
    let r = app
        .post(
            &format!("/api/jobs/{}/priority", ids[1]),
            json!({ "priority": 7 }),
        )
        .await;
    assert_eq!(r.json["priority"], 7);
    let r = app.get("/api/jobs?state=active").await;
    assert_eq!(r.json["items"][0]["id"], ids[1].as_str());
    let r = app
        .post(&format!("/api/jobs/{}/priority", ids[1]), json!({}))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app
        .post(
            &format!("/api/jobs/{}/priority", ids[1]),
            json!({ "move": "bottom" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Cancel a queued job: its file goes back to pending.
    let r = app
        .post_empty(&format!("/api/jobs/{}/cancel", ids[0]))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["state"], "cancelled");
    let file = app
        .get(&format!(
            "/api/files/{}",
            r.json["file_id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(file.json["file"]["status"], "pending");
    let r = app
        .post_empty(&format!("/api/jobs/{}/cancel", ids[0]))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.json["code"], "job_finished");
    let r = app
        .post(
            &format!("/api/jobs/{}/priority", ids[0]),
            json!({ "priority": 1 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);

    let r = app.get("/api/jobs?state=history").await;
    assert_eq!(r.json["total"], 1);
    let r = app
        .post("/api/jobs/clear", json!({ "state": "history" }))
        .await;
    assert_eq!(r.json["affected"], 1);
    let r = app.get("/api/jobs?state=history").await;
    assert_eq!(r.json["total"], 0);
    let r = app
        .post("/api/jobs/clear", json!({ "state": "queued" }))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = app.get("/api/jobs?state=weird").await;
    assert_eq!(r.json["code"], "invalid_state");
    let r = app
        .get(&format!("/api/jobs/{}", uuid::Uuid::new_v4()))
        .await;
    assert_eq!(r.json["code"], "job_not_found");
}

#[tokio::test]
async fn queue_pause_resume_and_stop() {
    let app = TestApp::new().await;
    app.fake.set_default(Behavior::Hold);
    app.write("Movies/a.mkv", h264());
    app.write("Movies/b.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let _ = lib;
    let app_ref = &app;
    wait_until("two running", || async {
        app_ref.get("/api/queue").await.json["running"] == 2
    })
    .await;
    let r = app.get("/api/queue").await;
    assert_eq!(r.json["paused"], false);
    assert_eq!(r.json["max_jobs"], 2);
    assert_eq!(r.json["max_jobs_auto"], true);

    // Stop now: running jobs go back to the queue and the queue pauses.
    let r = app.post_empty("/api/queue/stop").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["paused"], true);
    assert_eq!(r.json["running"], 0);
    assert_eq!(r.json["queued"], 2);
    let r = app.get("/api/files?status=queued").await;
    assert_eq!(r.json["total"], 2);

    // Resume picks them up again.
    app.fake.set_default(Behavior::Done { ratio: 0.5 });
    let r = app.post_empty("/api/queue/resume").await;
    assert_eq!(r.json["paused"], false);
    app.wait_queue_idle().await;
    let r = app.get("/api/files?status=done").await;
    assert_eq!(r.json["total"], 2);
    let r = app.post_empty("/api/queue/pause").await;
    assert_eq!(r.json["paused"], true);
    // The pause survives a restart.
    let dir = app.stop().await;
    let app = TestApp::start(TestOptions {
        dir: Some(dir),
        ..TestOptions::default()
    })
    .await;
    let r = app.get("/api/queue").await;
    assert_eq!(r.json["paused"], true);
}

#[tokio::test]
async fn cancel_running_job() {
    let app = TestApp::new().await;
    app.fake.set_default(Behavior::Hold);
    app.write("Movies/a.mkv", h264());
    app.add_library("Movies", json!({})).await;
    let app_ref = &app;
    wait_until("running", || async {
        app_ref.get("/api/jobs?state=running").await.json["total"] == 1
    })
    .await;
    let job = app.get("/api/jobs?state=running").await.json["items"][0].clone();
    assert_eq!(job["state"], "running");
    let file = app
        .get(&format!("/api/files/{}", job["file_id"].as_str().unwrap()))
        .await;
    assert_eq!(file.json["file"]["status"], "processing");

    let r = app
        .post_empty(&format!("/api/jobs/{}/cancel", job["id"].as_str().unwrap()))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json["state"], "cancelled");
    let file = app
        .get(&format!("/api/files/{}", job["file_id"].as_str().unwrap()))
        .await;
    assert_eq!(file.json["file"]["status"], "pending");
    assert!(
        app.media.join("Movies/a.mkv").exists(),
        "original untouched"
    );
}

#[tokio::test]
async fn skip_processing_file_cancels_it() {
    let app = TestApp::new().await;
    app.fake.set_default(Behavior::Hold);
    app.write("Movies/a.mkv", h264());
    app.add_library("Movies", json!({})).await;
    let app_ref = &app;
    wait_until("running", || async {
        app_ref.get("/api/files?status=processing").await.json["total"] == 1
    })
    .await;
    let id = app.get("/api/files?status=processing").await.json["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = app.post_empty(&format!("/api/files/{id}/skip")).await;
    assert_eq!(r.json["status"], "skipped");
    assert_eq!(r.json["skip_reason"], "Skipped by you");
    assert_eq!(app.state.dispatcher.running_count(), 0);
}

#[tokio::test]
async fn concurrent_queue_requests_create_one_job() {
    let (app, _lib) = fixture().await;
    let done_id = app.get("/api/files?q=done").await.json["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let path = format!("/api/files/{done_id}/queue");
    let (a, b, c) = tokio::join!(
        app.post_empty(&path),
        app.post_empty(&path),
        app.post_empty(&path)
    );
    let statuses = [a.status, b.status, c.status];
    assert_eq!(
        statuses.iter().filter(|s| **s == StatusCode::OK).count(),
        1,
        "{statuses:?}"
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        2,
        "{statuses:?}"
    );
    let r = app.get(&format!("/api/files/{done_id}")).await;
    assert_eq!(r.json["jobs"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn scan_writes_never_touch_busy_rows() {
    use crate::db::files::{FileUpsert, update_scanned};
    let (app, lib) = fixture().await;
    let queued = app.get("/api/files?status=queued&limit=1").await.json["items"][0].clone();
    let id = uuid::Uuid::parse_str(queued["id"].as_str().unwrap()).unwrap();
    let upsert = FileUpsert {
        library_id: uuid::Uuid::parse_str(&lib).unwrap(),
        path: queued["path"].as_str().unwrap().to_string(),
        relative_path: "x".into(),
        file_name: "x".into(),
        size_bytes: 1,
        modified_at: chrono::Utc::now(),
        status: chrysopoeia_core::FileStatus::Skipped,
        probe: None,
        skip_reason: Some("nope".into()),
        error: None,
    };
    let mut tx = app.state.db.write_tx().await.unwrap();
    let seen = crate::db::files::find_by_path_conn(&mut tx, &upsert.path)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(seen.id, id);
    assert!(!update_scanned(&mut tx, &seen, &upsert).await.unwrap());
    assert!(
        crate::db::files::insert(&mut tx, &upsert)
            .await
            .unwrap()
            .is_none(),
        "path conflict"
    );
    tx.commit().await.unwrap();
    let r = app.get(&format!("/api/files/{id}")).await;
    assert_eq!(r.json["file"]["status"], "queued");
    assert_eq!(r.json["file"]["file_name"], queued["file_name"]);
}

#[tokio::test]
async fn scan_writes_never_touch_rows_changed_since_they_were_read() {
    use crate::db::files::{FileUpsert, update_scanned};
    let (app, lib) = fixture().await;
    let failed = app.get("/api/files?status=failed&limit=1").await.json["items"][0].clone();
    let path = failed["path"].as_str().unwrap().to_string();
    let seen = crate::db::files::find_by_path(app.state.db.pool(), &path)
        .await
        .unwrap()
        .unwrap();
    // Something else writes the row after the scan read it.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let r = app
        .post_empty(&format!("/api/files/{}/skip", seen.id))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let upsert = FileUpsert {
        library_id: uuid::Uuid::parse_str(&lib).unwrap(),
        path: path.clone(),
        relative_path: "x".into(),
        file_name: "x".into(),
        size_bytes: 1,
        modified_at: chrono::Utc::now(),
        status: chrysopoeia_core::FileStatus::Failed,
        probe: None,
        skip_reason: None,
        error: Some("stale".into()),
    };
    let mut tx = app.state.db.write_tx().await.unwrap();
    assert!(!update_scanned(&mut tx, &seen, &upsert).await.unwrap());
    assert_eq!(
        crate::db::files::delete_unchanged(&mut tx, std::slice::from_ref(&seen))
            .await
            .unwrap(),
        0
    );
    tx.commit().await.unwrap();
    let r = app.get(&format!("/api/files/{}", seen.id)).await;
    assert_eq!(r.json["file"]["status"], "skipped");
    assert_eq!(r.json["file"]["skip_reason"], "Skipped by you");
}
