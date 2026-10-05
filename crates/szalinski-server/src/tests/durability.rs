//! Regression tests for what the end-to-end runs with the release binary
//! found: a minimum size that dropped converted files, changes made while
//! the server was down, a crash while a new file was being put in place,
//! two servers on one data folder, files that vanish during their job and
//! unusable stored ignore patterns.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;
use szalinski_core::Settings;
use szalinski_core::paths::backup_file_name;

use super::support::*;
use crate::app;

async fn activity_text(app: &TestApp) -> String {
    app.get("/api/activity?limit=100").await.json["items"].to_string()
}

/// A fake media file of about `bytes` bytes.
fn big_h264(bytes: usize) -> String {
    format!("{}pad={}\n", h264(), "#".repeat(bytes))
}

#[tokio::test]
async fn a_minimum_size_never_drops_files_already_in_the_list() {
    let app = TestApp::new().await;
    app.write("Movies/big.mkv", &big_h264(1_500_000));
    // Already AV1, so it stays above the minimum (and the folder never
    // looks empty).
    app.write(
        "Movies/other.mkv",
        &format!("{}pad={}\n", av1(), "#".repeat(1_500_000)),
    );
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    app.wait_queue_idle().await;
    let before = app.files_by_name(&id).await["big.mkv"].clone();
    assert_eq!(before["status"], "done");
    // Converted to about 0.75 MB: below the minimum set next.
    assert!(before["size_bytes"].as_u64().unwrap() < 1_000_000);
    let saved = app.get("/api/overview").await.json["totals"]["saved_bytes"].clone();

    app.write("Movies/sample.mkv", h264());
    let r = app
        .patch("/api/settings", json!({ "min_file_size_mb": 1 }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.rescan(&id).await;

    let files = app.files_by_name(&id).await;
    assert_eq!(files["big.mkv"]["status"], "done", "{files:#?}");
    assert_eq!(files["big.mkv"]["saved_bytes"], before["saved_bytes"]);
    assert!(
        !files.contains_key("sample.mkv"),
        "new small files stay out"
    );
    assert_eq!(
        app.get("/api/overview").await.json["totals"]["saved_bytes"],
        saved
    );
    assert!(!activity_text(&app).await.contains("removed"));

    // A watch event for the small converted file changes nothing either.
    let root = std::fs::canonicalize(app.media.join("Movies")).unwrap();
    app.fake
        .watch_sender()
        .send(szalinski_scanner::WatchEvent::Upserted(
            root.join("big.mkv"),
        ))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(app.files_by_name(&id).await["big.mkv"]["status"], "done");
}

#[tokio::test]
async fn an_unusable_stored_ignore_pattern_does_not_stop_removals() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/keep.mkv", h264());
    let gone = app.write("Movies/gone.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    // Saved before patterns were checked this strictly (the API refuses it).
    app.state.replace_settings(Settings {
        ignore_patterns: vec!["!Extras".into()],
        ..app.state.settings()
    });
    std::fs::remove_file(gone).unwrap();
    app.rescan(&id).await;
    let files = app.files_by_name(&id).await;
    assert!(!files.contains_key("gone.mkv"), "{files:#?}");
    assert!(files.contains_key("keep.mkv"));

    // It doesn't block other changes either; a new bad pattern is refused.
    let r = app
        .patch("/api/settings", json!({ "watch_folders": false }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = app
        .patch(
            "/api/settings",
            json!({ "ignore_patterns": ["!Extras", "C:\\Temp"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    assert_eq!(r.json["field"], "ignore_patterns");
}

#[tokio::test]
async fn a_file_deleted_during_its_job_leaves_the_list() {
    let app = TestApp::new().await;
    app.fake.set_behavior(
        "gone.mkv",
        Behavior::HoldFail("The file no longer exists".into()),
    );
    app.write("Movies/other.mkv", av1());
    let gone = app.write("Movies/gone.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    let fake = Arc::clone(&app.fake);
    wait_until("the job to start", move || {
        let fake = Arc::clone(&fake);
        async move { !fake.started().is_empty() }
    })
    .await;
    std::fs::remove_file(gone).unwrap();
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;

    let files = app.files_by_name(&id).await;
    assert!(!files.contains_key("gone.mkv"), "{files:#?}");
    let feed = activity_text(&app).await;
    assert!(
        feed.contains("gone.mkv was moved or deleted while it was being converted"),
        "{feed}"
    );
    assert!(!feed.contains("Failed gone.mkv"), "{feed}");
}

#[tokio::test]
async fn changes_made_while_the_server_was_down_are_found_at_start() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/old.mkv", h264());
    let removed = app.write("Movies/removed.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    let dir = app.stop().await;

    // While it's down: a new file, and one deleted.
    std::fs::write(dir.path().join("media/Movies/new.mkv"), h264()).unwrap();
    std::fs::remove_file(removed).unwrap();

    let fake = Arc::new(FakeToolkit::default());
    let app = TestApp::start(TestOptions {
        dir: Some(dir),
        fake: Arc::clone(&fake),
        ..TestOptions::default()
    })
    .await;
    let app_ref = &app;
    let id_ref = &id;
    wait_until("the catch-up scan", || async move {
        let files = app_ref.files_by_name(id_ref).await;
        files.contains_key("new.mkv") && !files.contains_key("removed.mkv")
    })
    .await;
    app.wait_scan(&id).await;
    // One walk: the scan's. A clean stop leaves no leftovers to look for.
    assert_eq!(fake.walks.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn catch_up_scans_are_off_when_nothing_is_found_automatically() {
    let app = TestApp::new().await;
    app.pause().await;
    let r = app
        .patch(
            "/api/settings",
            json!({ "watch_folders": false, "rescan_interval_hours": 0 }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.add_library("Movies", json!({})).await;
    let dir = app.stop().await;
    let fake = Arc::new(FakeToolkit::default());
    let app = TestApp::start(TestOptions {
        dir: Some(dir),
        fake: Arc::clone(&fake),
        ..TestOptions::default()
    })
    .await;
    let state = app.state.clone();
    wait_until("dispatcher ready", move || {
        let state = state.clone();
        async move { state.dispatcher.is_ready() }
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(fake.walks.lock().unwrap().is_empty());
}

/// Put a job into the state a crash in the middle of replacing `clip.mp4`
/// with `clip.mkv` leaves: running, finalizing, its final path recorded.
async fn crash_while_finalizing(new_file_in_place: bool) -> (TestApp, String, String) {
    let app = TestApp::new().await;
    app.pause().await;
    let input = app.write("Movies/clip.mp4", &big_h264(10_000));
    let lib = app.add_library("Movies", json!({})).await;
    let job = app.get("/api/jobs?state=queued").await.json["items"][0].clone();
    let job_id = job["id"].as_str().unwrap().to_string();
    let file_id = job["file_id"].as_str().unwrap().to_string();
    let input = std::fs::canonicalize(input).unwrap();
    let final_path = input.with_extension("mkv");
    let pool = app.state.db.pool().clone();
    sqlx::query(
        "UPDATE jobs SET state = 'running', stage = 'finalizing', encoder = 'libsvtav1', \
         hw_api = 'software', attempt = 1, final_path = ? WHERE id = ?",
    )
    .bind(final_path.to_str().unwrap())
    .bind(&job_id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE files SET status = 'processing' WHERE id = ?")
        .bind(&file_id)
        .execute(&pool)
        .await
        .unwrap();
    // The original was moved aside; the new file may already be in place.
    let uuid = uuid::Uuid::parse_str(&job_id).unwrap();
    let backup = input.with_file_name(backup_file_name("clip.mp4", uuid));
    std::fs::rename(&input, &backup).unwrap();
    if new_file_in_place {
        std::fs::write(&final_path, av1()).unwrap();
    }
    let _ = lib;
    // The process dies: no clean shutdown.
    app.state.shutdown.cancel();
    let dir = app.dir;
    let app = TestApp::start(TestOptions {
        dir: Some(dir),
        ..TestOptions::default()
    })
    .await;
    (app, job_id, file_id)
}

#[tokio::test]
async fn a_crash_after_the_new_file_went_in_finishes_the_replacement() {
    let (app, job_id, file_id) = crash_while_finalizing(true).await;
    assert_eq!(app.startup.requeued_jobs, 0);
    let job = app.get(&format!("/api/jobs/{job_id}")).await.json;
    assert_eq!(job["state"], "done", "{job}");
    assert!(
        job["notes"][0]
            .as_str()
            .unwrap()
            .contains("already complete"),
        "{job}"
    );
    let file = app.get(&format!("/api/files/{file_id}")).await.json["file"].clone();
    assert_eq!(file["status"], "done", "{file}");
    assert_eq!(file["file_name"], "clip.mkv");
    assert!(file["saved_bytes"].as_i64().unwrap() > 0, "{file}");
    // Only the new file is left: no original beside it, no backup.
    let movies = std::fs::canonicalize(app.media.join("Movies")).unwrap();
    let mut names: Vec<String> = std::fs::read_dir(&movies)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["clip.mkv"]);
    assert!(activity_text(&app).await.contains("Converted clip.mp4"));
}

#[tokio::test]
async fn a_crash_before_the_new_file_went_in_restores_the_original_and_runs_again() {
    let (app, job_id, file_id) = crash_while_finalizing(false).await;
    assert_eq!(app.startup.requeued_jobs, 1);
    let job = app.get(&format!("/api/jobs/{job_id}")).await.json;
    assert_eq!(job["state"], "queued", "{job}");
    let movies = std::fs::canonicalize(app.media.join("Movies")).unwrap();
    let movies_c = movies.clone();
    wait_until("the backup to be put back", move || {
        let movies = movies_c.clone();
        async move { movies.join("clip.mp4").exists() }
    })
    .await;
    app.resume().await;
    app.wait_queue_idle().await;
    let file = app.get(&format!("/api/files/{file_id}")).await.json["file"].clone();
    assert_eq!(file["status"], "done", "{file}");
    assert_eq!(file["file_name"], "clip.mkv");
    assert!(!movies.join("clip.mp4").exists());
}

#[tokio::test]
async fn a_second_server_cannot_use_the_same_data_folder() {
    let dir = tempfile::tempdir().unwrap();
    let first = app::lock_data_dir(dir.path()).unwrap();
    let err = app::lock_data_dir(dir.path()).unwrap_err().to_string();
    assert!(
        err.contains("Another Szalinski is already using the data folder"),
        "{err}"
    );
    drop(first);
    app::lock_data_dir(dir.path()).unwrap();
}

/// An older image (Chrysopoeia, the name before the rename) holds only its
/// own lock file: this server holds that one too, so the two can never
/// share a data folder by mistake, in either order.
#[tokio::test]
async fn an_older_image_on_the_same_data_folder_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.path().join(app::LEGACY_LOCK_FILE_NAME))
        .unwrap();
    rustix::fs::flock(
        &legacy,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .unwrap();
    let e = app::lock_data_dir_in(dir.path(), true)
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("Another Szalinski is already using the data folder"),
        "{e}"
    );
    drop(legacy);

    let _held = app::lock_data_dir_in(dir.path(), true).unwrap();
    let legacy = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.path().join(app::LEGACY_LOCK_FILE_NAME))
        .unwrap();
    assert!(
        rustix::fs::flock(
            &legacy,
            rustix::fs::FlockOperation::NonBlockingLockExclusive
        )
        .is_err(),
        "the older image's lock is held too"
    );
}

/// In a container the data folder is set by the image and must be left
/// alone, so the advice names the Config folder (the `/config` mount);
/// outside one it names the option that moves the data folder.
#[tokio::test]
async fn the_data_folder_advice_depends_on_where_the_server_runs() {
    let dir = tempfile::tempdir().unwrap();
    let _first = app::lock_data_dir_in(dir.path(), true).unwrap();

    let in_docker = app::lock_data_dir_in(dir.path(), true)
        .unwrap_err()
        .to_string();
    assert!(
        in_docker.contains("Stop the other one first, or give this one its own Config folder"),
        "{in_docker}"
    );
    assert!(
        in_docker.contains("Unraid: the Config path; docker: -v /other/folder:/config"),
        "{in_docker}"
    );
    assert!(!in_docker.contains("DATA_DIR"), "{in_docker}");
    assert!(!in_docker.contains("--data-dir"), "{in_docker}");

    let outside = app::lock_data_dir_in(dir.path(), false)
        .unwrap_err()
        .to_string();
    assert!(
        outside.contains("Stop the other one first, or give this one its own data folder"),
        "{outside}"
    );
    assert!(outside.contains("--data-dir"), "{outside}");
    assert!(outside.contains("DATA_DIR"), "{outside}");
    assert!(!outside.contains("Config folder"), "{outside}");
    for text in [&in_docker, &outside] {
        assert!(text.contains(&dir.path().display().to_string()), "{text}");
    }
}
