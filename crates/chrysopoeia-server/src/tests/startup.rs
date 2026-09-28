//! Start-up: first-run options, recovery of interrupted work, leftovers.

use axum::http::StatusCode;
use chrysopoeia_core::paths::{backup_file_name, temp_file_name};
use serde_json::json;

use super::support::*;
use crate::db;

#[tokio::test]
async fn first_run_applies_command_line_options() {
    let fake = std::sync::Arc::new(FakeToolkit::default());
    let app = TestApp::start(TestOptions {
        fake,
        configure: Box::new(|c, root| {
            let movies = root.join("media/Movies");
            std::fs::create_dir_all(&movies).unwrap();
            std::fs::write(movies.join("a.mkv"), h264()).unwrap();
            c.hw = chrysopoeia_core::HwPreference::Nvenc;
            c.max_jobs = Some(3);
            c.libraries = vec![movies, root.join("media/missing")];
        }),
        ..TestOptions::default()
    })
    .await;
    let s = app.get("/api/settings").await;
    assert_eq!(s.json["hardware"], "nvenc");
    // MAX_JOBS takes the place of the automatic count; it isn't saved.
    assert_eq!(s.json["max_jobs"], serde_json::Value::Null);
    let q = app.get("/api/queue").await;
    assert_eq!(q.json["max_jobs"], 3);
    assert_eq!(q.json["max_jobs_auto"], false);
    let libs = app.get("/api/libraries").await;
    let libs = libs.json.as_array().unwrap();
    assert_eq!(
        libs.len(),
        1,
        "the missing folder is skipped with a warning"
    );
    assert_eq!(libs[0]["name"], "Movies");
    let id = libs[0]["id"].as_str().unwrap().to_string();
    app.wait_scan(&id).await;
    app.wait_queue_idle().await;
    assert_eq!(app.get("/api/files?status=done").await.json["total"], 1);

    // A later start with changed values: HW_ACCEL applies because it
    // changed, MAX_JOBS applies because "Files at once" is still automatic,
    // and libraries aren't added again.
    let dir = app.stop().await;
    let app = TestApp::start(TestOptions {
        dir: Some(dir),
        configure: Box::new(|c, _| {
            c.hw = chrysopoeia_core::HwPreference::Cpu;
            c.max_jobs = Some(9);
            c.libraries = vec![];
        }),
        ..TestOptions::default()
    })
    .await;
    let s = app.get("/api/settings").await;
    assert_eq!(s.json["hardware"], "cpu");
    let q = app.get("/api/queue").await;
    assert_eq!(q.json["max_jobs"], 9);
    assert_eq!(q.json["max_jobs_auto"], false);
    assert_eq!(q.json["max_jobs_source"], "env");
    let feed = app.get("/api/activity").await.json["items"].to_string();
    assert!(feed.contains("HW_ACCEL=cpu"), "{feed}");
    assert_eq!(
        app.get("/api/libraries")
            .await
            .json
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Choices made in Settings win while the variables stay the same.
    let r = app
        .patch(
            "/api/settings",
            serde_json::json!({ "hardware": "auto", "max_jobs": 2 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let dir = app.stop().await;
    let app = TestApp::start(TestOptions {
        dir: Some(dir),
        configure: Box::new(|c, _| {
            c.hw = chrysopoeia_core::HwPreference::Cpu;
            c.max_jobs = Some(9);
        }),
        ..TestOptions::default()
    })
    .await;
    let s = app.get("/api/settings").await;
    assert_eq!(s.json["hardware"], "auto");
    let q = app.get("/api/queue").await;
    assert_eq!(q.json["max_jobs"], 2);
    assert_eq!(q.json["max_jobs_source"], "settings");
    let feed = app.get("/api/activity").await.json["items"].to_string();
    // Named as the UI names the setting.
    assert!(
        feed.contains(
            "MAX_JOBS=9 is not used because Files at once is set to 2 in Settings. Choose \
             Automatic there to use MAX_JOBS."
        ),
        "{feed}"
    );
}

#[tokio::test]
async fn interrupted_jobs_are_requeued_and_leftovers_recovered() {
    // First run: a library with a job that is "running" when the process dies.
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/a.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let lib_id = lib["id"].as_str().unwrap().to_string();
    let job = app.get("/api/jobs?state=queued").await.json["items"][0].clone();
    let job_id = job["id"].as_str().unwrap().to_string();
    let file_id = job["file_id"].as_str().unwrap().to_string();
    let pool = app.state.db.pool().clone();
    sqlx::query("UPDATE jobs SET state = 'running', stage = 'transcoding', progress = 40, attempt = 2 WHERE id = ?")
        .bind(&job_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE files SET status = 'processing' WHERE id = ?")
        .bind(&file_id)
        .execute(&pool)
        .await
        .unwrap();

    // Crash leftovers: a temp file in the temp dir, and in the library a
    // backup whose original is missing (restore it) plus a stale temp.
    let tmp = app.dir.path().join("scratch");
    std::fs::create_dir_all(&tmp).unwrap();
    let id = uuid::Uuid::new_v4();
    let stale_tmp = tmp.join(temp_file_name("x", id, "mkv"));
    std::fs::write(&stale_tmp, "partial").unwrap();
    let movies = std::fs::canonicalize(app.media.join("Movies")).unwrap();
    let backup = movies.join(backup_file_name("b.mkv", id));
    std::fs::write(&backup, h264()).unwrap();
    let lib_tmp = movies.join(temp_file_name("a", id, "mkv"));
    std::fs::write(&lib_tmp, "partial").unwrap();
    // Folder mode without a temp folder encodes into the output folder.
    let out = app.dir.path().join("converted");
    std::fs::create_dir_all(out.join("Movies")).unwrap();
    let r = app
        .patch(
            "/api/settings",
            json!({ "output_mode": "folder", "output_folder": out.to_str().unwrap() }),
        )
        .await;
    assert_eq!(r.status, axum::http::StatusCode::OK, "{}", r.text);
    let out_tmp = out.join("Movies").join(temp_file_name("c", id, "mkv"));
    std::fs::write(&out_tmp, "partial").unwrap();

    // Simulate a crash: no clean shutdown flag, no graceful stop.
    app.state.shutdown.cancel();
    let dir = app.dir;
    let fake = std::sync::Arc::new(FakeToolkit::default());
    let tmp_c = tmp.clone();
    let app = TestApp::start(TestOptions {
        dir: Some(dir),
        fake: fake.clone(),
        configure: Box::new(move |c, _| c.temp_dir = Some(tmp_c)),
        ..TestOptions::default()
    })
    .await;
    assert_eq!(app.startup.requeued_jobs, 1);
    assert!(!app.startup.clean_shutdown);
    let j = app.get(&format!("/api/jobs/{job_id}")).await;
    assert_eq!(j.json["state"], "queued");
    assert_eq!(j.json["attempt"], 0);
    assert_eq!(j.json["progress"], 0.0);
    let f = app.get(&format!("/api/files/{file_id}")).await;
    assert_eq!(f.json["file"]["status"], "queued");

    let fake_c = fake.clone();
    wait_until("leftovers handled", move || {
        let fake = fake_c.clone();
        async move { fake.recovered.lock().unwrap().len() == 4 }
    })
    .await;
    assert!(!stale_tmp.exists());
    assert!(!lib_tmp.exists());
    assert!(!out_tmp.exists(), "leftover in the output folder");
    assert!(!backup.exists());
    assert!(movies.join("b.mkv").exists(), "backup restored");
    let act = app.get("/api/activity").await;
    assert!(act.text.contains("Restored"), "{}", act.text);
    assert!(
        act.text.contains("restarted while 1 job was running"),
        "{}",
        act.text
    );

    // Resuming runs the recovered job.
    app.resume().await;
    app.wait_queue_idle().await;
    let f = app.get(&format!("/api/files/{file_id}")).await;
    assert_eq!(f.json["file"]["status"], "done");
    let _ = lib_id;
}

#[tokio::test]
async fn clean_shutdown_skips_the_leftover_search() {
    let app = TestApp::new().await;
    let lib = app.add_library("Movies", json!({})).await;
    let dir = app.stop().await;
    let fake = std::sync::Arc::new(FakeToolkit::default());
    let app = TestApp::start(TestOptions {
        dir: Some(dir),
        fake: fake.clone(),
        ..TestOptions::default()
    })
    .await;
    assert!(app.startup.clean_shutdown);
    let fake_c = fake.clone();
    wait_until("the catch-up scan", move || {
        let fake = fake_c.clone();
        async move { !fake.walks.lock().unwrap().is_empty() }
    })
    .await;
    app.wait_scan(lib["id"].as_str().unwrap()).await;
    // The catch-up scan walks the library once; a clean stop leaves no
    // leftovers, so recovery doesn't walk it again.
    assert_eq!(fake.walks.lock().unwrap().len(), 1);
    assert!(fake.recovered.lock().unwrap().is_empty());
}

#[tokio::test]
async fn activity_is_paged_newest_first() {
    let app = TestApp::new().await;
    for i in 0..5 {
        app.state
            .activity(
                chrysopoeia_core::ActivityLevel::Info,
                format!("entry {i}"),
                db::activity::ActivityRefs::default(),
            )
            .await;
    }
    let r = app.get("/api/activity?limit=2").await;
    assert_eq!(r.status, StatusCode::OK);
    let items = r.json["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["message"], "entry 4");
    let before = items[1]["id"].as_i64().unwrap();
    let r = app
        .get(&format!("/api/activity?limit=2&before={before}"))
        .await;
    assert_eq!(r.json["items"][0]["message"], "entry 2");
}

#[tokio::test]
async fn activity_is_trimmed() {
    let app = TestApp::new().await;
    let pool = app.state.db.pool().clone();
    sqlx::query(
        "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5100) \
         INSERT INTO activity (at, level, message) SELECT '2026-01-01T00:00:00.000Z', 'info', 'old' FROM n",
    )
    .execute(&pool)
    .await
    .unwrap();
    app.state
        .activity(
            chrysopoeia_core::ActivityLevel::Info,
            "newest",
            db::activity::ActivityRefs::default(),
        )
        .await;
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM activity")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, db::activity::KEEP_ROWS);
}

/// With the real media crates (unfinished stubs that panic, or the real
/// thing once merged) the server keeps answering and never crashes.
#[tokio::test]
async fn real_toolkit_never_takes_the_server_down() {
    let app = TestApp::start(TestOptions {
        real_toolkit: true,
        wait_ready: false,
        ..TestOptions::default()
    })
    .await;
    let state = app.state.clone();
    wait_until_for(
        "hardware check",
        std::time::Duration::from_secs(90),
        move || {
            let state = state.clone();
            async move { state.hardware.is_ready() }
        },
    )
    .await;
    std::fs::create_dir_all(app.media.join("Movies")).unwrap();
    let r = app
        .post(
            "/api/libraries",
            json!({ "path": app.media.join("Movies").to_str().unwrap() }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    app.wait_scan(r.json["id"].as_str().unwrap()).await;
    for path in [
        "/api/health",
        "/api/libraries",
        "/api/hardware",
        "/api/overview",
        "/api/presets",
        "/api/fs/browse",
        "/api/activity",
    ] {
        let r = app.get(path).await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text);
    }
}
