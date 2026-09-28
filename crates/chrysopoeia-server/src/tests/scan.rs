//! Scanning and watch events.

use std::sync::atomic::Ordering;
use std::time::Duration;

use chrysopoeia_scanner::WatchEvent;
use serde_json::json;

use super::support::*;

fn set_mtime_later(path: &std::path::Path) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    let t = std::time::SystemTime::now() + Duration::from_secs(120);
    f.set_modified(t).unwrap();
}

#[tokio::test]
async fn scan_new_changed_unchanged_removed() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/new.mkv", h264());
    app.write("Movies/sub/already.mkv", av1());
    app.write("Movies/broken.mkv", "broken");
    app.write("Movies/song.flac", audio_only());
    app.write("Movies/notes.txt", "not media");
    app.write("Movies/.hidden.mkv", h264());
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let id = lib["id"].as_str().unwrap().to_string();
    assert_eq!(lib["stats"]["file_count"], 4);
    assert_eq!(app.fake.probes.load(Ordering::SeqCst), 4);

    let files = app.files_by_name(&id).await;
    assert_eq!(files["new.mkv"]["status"], "queued");
    assert_eq!(files["new.mkv"]["video_codec"], "h264");
    assert_eq!(files["new.mkv"]["resolution"], "1080p");
    assert_eq!(files["new.mkv"]["container"], "matroska");
    assert_eq!(files["new.mkv"]["relative_path"], "new.mkv");
    assert_eq!(files["already.mkv"]["status"], "skipped");
    assert_eq!(files["already.mkv"]["skip_reason"], "Already AV1");
    assert_eq!(files["already.mkv"]["relative_path"], "sub/already.mkv");
    assert_eq!(files["broken.mkv"]["status"], "failed");
    let err = files["broken.mkv"]["error"].as_str().unwrap();
    assert!(err.contains("can't be read"), "{err}");
    assert!(files["broken.mkv"]["video_codec"].is_null());
    assert_eq!(files["song.flac"]["status"], "skipped");
    assert!(
        files["new.mkv"].get("probe").is_none(),
        "lists omit the probe"
    );

    // Unchanged files are not probed again.
    app.rescan(&id).await;
    assert_eq!(app.fake.probes.load(Ordering::SeqCst), 4);

    // A changed file is re-probed and re-decided; a removed one disappears.
    let already = app.write(
        "Movies/sub/already.mkv",
        "video=mpeg2video\naudio=mp2\nwidth=720\nheight=576\n",
    );
    set_mtime_later(&already);
    std::fs::remove_file(app.media.join("Movies/song.flac")).unwrap();
    app.rescan(&id).await;
    assert_eq!(app.fake.probes.load(Ordering::SeqCst), 5);
    let files = app.files_by_name(&id).await;
    assert_eq!(files.len(), 3);
    assert!(!files.contains_key("song.flac"));
    assert_eq!(files["already.mkv"]["status"], "queued");
    assert_eq!(files["already.mkv"]["resolution"], "576p");
    assert!(files["already.mkv"]["skip_reason"].is_null());

    let act = app.get("/api/activity").await;
    let first = act.json["items"][0]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        first.starts_with("Scanned Movies: 3 files, 2 need converting"),
        "{first}"
    );
    assert!(first.contains("1 removed"), "{first}");
}

#[tokio::test]
async fn empty_walk_keeps_rows_and_warns() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/a.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    std::fs::remove_file(app.media.join("Movies/a.mkv")).unwrap();
    app.rescan(&id).await;
    assert_eq!(app.files_by_name(&id).await.len(), 1);
    let act = app.get("/api/activity?limit=5").await;
    assert!(
        act.json["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["level"] == "warning"
                && e["message"].as_str().unwrap().contains("looks empty")),
        "{}",
        act.text
    );
}

#[tokio::test]
async fn auto_queue_off_leaves_files_pending() {
    let app = TestApp::new().await;
    let r = app
        .patch("/api/settings", json!({ "auto_queue": false }))
        .await;
    assert_eq!(r.status, axum::http::StatusCode::OK);
    app.write("Movies/a.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let files = app.files_by_name(lib["id"].as_str().unwrap()).await;
    assert_eq!(files["a.mkv"]["status"], "pending");
    assert!(files["a.mkv"]["job_id"].is_null());
    let r = app.get("/api/jobs").await;
    assert_eq!(r.json["total"], 0);
}

#[tokio::test]
async fn ignore_patterns_and_min_size() {
    let app = TestApp::new().await;
    app.pause().await;
    let r = app
        .patch(
            "/api/settings",
            json!({ "ignore_patterns": ["**/Extras/**"], "min_file_size_mb": 0 }),
        )
        .await;
    assert_eq!(r.status, axum::http::StatusCode::OK, "{}", r.text);
    app.write("Movies/a.mkv", h264());
    app.write("Movies/Film/Extras/trailer.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let files = app.files_by_name(lib["id"].as_str().unwrap()).await;
    assert_eq!(files.len(), 1);
    assert!(files.contains_key("a.mkv"));
}

#[tokio::test]
async fn watch_events_upsert_and_remove() {
    let app = TestApp::new().await;
    app.pause().await;
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    let root = std::fs::canonicalize(app.media.join("Movies")).unwrap();
    // The watcher follows enabled libraries.
    let fake = app.fake.clone();
    let root_c = root.clone();
    wait_until("root watched", move || {
        let fake = fake.clone();
        let root = root_c.clone();
        async move { fake.watched.lock().unwrap().contains(&root) }
    })
    .await;

    let tx = app.fake.watch_sender();
    let path = root.join("Season 1/ep1.mkv");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, h264()).unwrap();
    tx.send(WatchEvent::Upserted(path.clone())).await.unwrap();
    let app_ref = &app;
    let id_c = id.clone();
    wait_until("watched file stored", || async {
        app_ref.files_by_name(&id_c).await.contains_key("ep1.mkv")
    })
    .await;
    let files = app.files_by_name(&id).await;
    assert_eq!(files["ep1.mkv"]["status"], "queued");
    assert_eq!(files["ep1.mkv"]["relative_path"], "Season 1/ep1.mkv");

    // An event for a queued file is ignored (no new probe).
    let probes = app.fake.probes.load(Ordering::SeqCst);
    tx.send(WatchEvent::Upserted(path.clone())).await.unwrap();
    // Events outside every library are ignored too.
    tx.send(WatchEvent::Upserted(app.dir.path().join("elsewhere.mkv")))
        .await
        .unwrap();
    // Removing the folder removes its files.
    std::fs::remove_dir_all(root.join("Season 1")).unwrap();
    tx.send(WatchEvent::Removed(root.join("Season 1")))
        .await
        .unwrap();
    wait_until("watched file removed", || async {
        !app_ref.files_by_name(&id_c).await.contains_key("ep1.mkv")
    })
    .await;
    assert_eq!(app.fake.probes.load(Ordering::SeqCst), probes);

    // Disabling the library stops watching it.
    app.patch(&format!("/api/libraries/{id}"), json!({ "enabled": false }))
        .await;
    assert!(!app.fake.watched.lock().unwrap().contains(&root));
    // Turning watching off drops the watcher.
    app.patch(&format!("/api/libraries/{id}"), json!({ "enabled": true }))
        .await;
    assert!(app.fake.watched.lock().unwrap().contains(&root));
    app.patch("/api/settings", json!({ "watch_folders": false }))
        .await;
    assert!(app.fake.watched.lock().unwrap().is_empty());
}

#[tokio::test]
async fn large_library_is_scanned_in_batches() {
    let app = TestApp::new().await;
    app.pause().await;
    for i in 0..600 {
        app.write(&format!("Shows/Season {}/ep{i:04}.mkv", i / 50), h264());
    }
    let lib = app.add_library("Shows", json!({})).await;
    assert_eq!(lib["stats"]["file_count"], 600);
    assert_eq!(lib["stats"]["queued"], 600);
    let r = app.get("/api/jobs?state=queued&limit=1").await;
    assert_eq!(r.json["total"], 600);
    let r = app.get("/api/files?limit=500&offset=500").await;
    assert_eq!(r.json["items"].as_array().unwrap().len(), 100);
    assert_eq!(r.json["total"], 600);
    // A second scan probes nothing.
    let probes = app.fake.probes.load(Ordering::SeqCst);
    app.rescan(lib["id"].as_str().unwrap()).await;
    assert_eq!(app.fake.probes.load(Ordering::SeqCst), probes);
}
