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
    let feed = app.get("/api/activity").await.json["items"].clone();
    // Named as the UI names the setting, once, as a warning (the feed entry
    // is also the one log line).
    let notes: Vec<&serde_json::Value> = feed
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            e["message"]
                .as_str()
                .unwrap_or("")
                .starts_with("MAX_JOBS=9")
        })
        .collect();
    assert_eq!(notes.len(), 1, "{feed}");
    assert_eq!(
        notes[0]["message"],
        "MAX_JOBS=9 is not used because Files at once is set to 2 in Settings. Choose \
         Automatic there to use MAX_JOBS."
    );
    assert_eq!(notes[0]["level"], "warning");
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

/// A library with a job that was putting its new file in place when the
/// server died: the original is moved aside as the job's backup, and the
/// job's temp file is next to it. Returns the app (paused), the job id, and
/// the original's, backup's and temp file's paths.
async fn crashed_while_replacing() -> (TestApp, uuid::Uuid, [std::path::PathBuf; 3]) {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/a.mkv", h264());
    app.write("Movies/other.mkv", av1());
    app.add_library("Movies", json!({})).await;
    let job = app.get("/api/jobs?state=queued").await.json["items"][0].clone();
    let job_id: uuid::Uuid = job["id"].as_str().unwrap().parse().unwrap();
    let movies = std::fs::canonicalize(app.media.join("Movies")).unwrap();
    let original = movies.join("a.mkv");
    let pool = app.state.db.pool().clone();
    sqlx::query(
        "UPDATE jobs SET state = 'running', stage = 'finalizing', final_path = ? WHERE id = ?",
    )
    .bind(original.to_str().unwrap())
    .bind(job_id.to_string())
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE files SET status = 'processing' WHERE id = ?")
        .bind(job["file_id"].as_str().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let backup = movies.join(backup_file_name("a.mkv", job_id));
    std::fs::rename(&original, &backup).unwrap();
    let temp = movies.join(temp_file_name("a", job_id, "mkv"));
    std::fs::write(&temp, "partial").unwrap();
    (app, job_id, [original, backup, temp])
}

/// The job's own backup is put back at the start even when the last stop
/// was recorded as clean, so the library folders aren't searched.
#[tokio::test]
async fn an_interrupted_jobs_backup_is_put_back_whatever_the_last_stop_recorded() {
    let (app, _, [original, backup, temp]) = crashed_while_replacing().await;
    db::settings::set_flag(app.state.db.pool(), db::settings::CLEAN_SHUTDOWN_KEY, true)
        .await
        .unwrap();
    app.state.shutdown.cancel();
    let app = TestApp::start(TestOptions {
        dir: Some(app.dir),
        ..TestOptions::default()
    })
    .await;
    assert!(app.startup.clean_shutdown);
    assert!(original.exists(), "original put back");
    assert!(!backup.exists());
    assert!(!temp.exists());
    let act = app.get("/api/activity").await;
    assert!(act.text.contains("Restored"), "{}", act.text);
    app.resume().await;
    app.wait_queue_idle().await;
    assert_eq!(app.get("/api/files?status=done").await.json["total"], 1);
}

/// A library that is out of reach at the start keeps the leftover search
/// due: the next stop isn't recorded as clean, and the job puts its own
/// backup back when it runs once the folder is back.
#[tokio::test]
async fn a_library_out_of_reach_at_the_start_keeps_its_leftovers_searched_for() {
    let (app, _, [original, backup, _]) = crashed_while_replacing().await;
    let movies = original.parent().unwrap().to_path_buf();
    let away = movies.with_file_name("Movies.away");
    app.state.shutdown.cancel();
    let dir = app.dir;
    std::fs::rename(&movies, &away).unwrap();
    let app = TestApp::start(TestOptions {
        dir: Some(dir),
        ..TestOptions::default()
    })
    .await;
    assert!(!app.startup.clean_shutdown);
    let state = app.state.clone();
    wait_until("the start-up search", move || {
        let state = state.clone();
        async move { !crate::state::lock(&state.leftovers).searching }
    })
    .await;
    assert!(crate::state::lock(&app.state.leftovers).due());

    // Stopped again before the library came back: not a clean stop, so
    // the next start searches the library folders.
    let dir = app.stop().await;
    std::fs::rename(&away, &movies).unwrap();
    let app = TestApp::start(TestOptions {
        dir: Some(dir),
        ..TestOptions::default()
    })
    .await;
    assert!(
        !app.startup.clean_shutdown,
        "a stop with leftovers still due isn't clean"
    );
    assert!(original.exists(), "put back at the start");
    assert!(!backup.exists());
    app.resume().await;
    app.wait_queue_idle().await;
    assert_eq!(app.get("/api/files?status=done").await.json["total"], 1);
}

/// The job itself puts back what its interrupted run moved aside when the
/// start-up could not (its folder was out of reach then, and the last stop
/// was recorded as clean, so the library isn't searched).
#[tokio::test]
async fn a_job_puts_back_the_original_its_interrupted_run_moved_aside() {
    let (app, job_id, [original, backup, _]) = crashed_while_replacing().await;
    // No scans at the start that could find the backup first.
    let r = app
        .patch(
            "/api/settings",
            json!({ "watch_folders": false, "rescan_interval_hours": 0 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    db::settings::set_flag(app.state.db.pool(), db::settings::CLEAN_SHUTDOWN_KEY, true)
        .await
        .unwrap();
    app.state.shutdown.cancel();
    let app = TestApp::start(TestOptions {
        dir: Some(app.dir),
        background: false,
        ..TestOptions::default()
    })
    .await;
    // As if the folder had been out of reach at the start.
    std::fs::rename(&original, &backup).unwrap();
    crate::app::start_background(&app.state, app.startup);
    app.wait_ready().await;
    app.resume().await;
    app.wait_queue_idle().await;
    assert!(original.exists());
    assert!(!backup.exists());
    let j = app.get(&format!("/api/jobs/{job_id}")).await;
    assert_eq!(j.json["state"], "done", "{}", j.json);
}

/// A damaged database used to stop every start with advice about folder
/// permissions (a restart loop under Docker). It is moved aside instead,
/// and the server starts with a new one and says so.
#[tokio::test]
async fn a_damaged_database_is_moved_aside_at_start() {
    let app = TestApp::new().await;
    app.add_library("Movies", json!({})).await;
    let dir = app.stop().await;
    let data = dir.path().join("data");
    let path = data.join(db::DB_FILE_NAME);
    // SQLite closes its connections on their own threads; the last one
    // folds the log into the file (and removes the log) a moment after the
    // pool says it is closed. Damage the file only after that.
    let wal = data.join(format!("{}-wal", db::DB_FILE_NAME));
    wait_until("the database is closed", || async { !wal.exists() }).await;
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.truncate(bytes.len().min(4096));
    bytes[..16].copy_from_slice(b"garbage-garbage!");
    std::fs::write(&path, &bytes).unwrap();

    let app = TestApp::start(TestOptions {
        dir: Some(dir),
        ..TestOptions::default()
    })
    .await;
    let aside: Vec<String> = std::fs::read_dir(&data)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("chrysopoeia.db.damaged-"))
        .collect();
    assert_eq!(aside.len(), 1, "{aside:?}");
    let act = app.get("/api/activity").await;
    assert!(
        act.text.contains("The database was damaged") && act.text.contains(&aside[0]),
        "{}",
        act.text
    );
    assert!(act.text.contains("media files were not touched"));
    assert_eq!(
        app.get("/api/libraries")
            .await
            .json
            .as_array()
            .unwrap()
            .len(),
        0
    );
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
