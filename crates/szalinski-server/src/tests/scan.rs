//! Scanning and watch events.

use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::json;
use szalinski_core::Settings;
use szalinski_scanner::WatchEvent;

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
    // One clean sentence, not a sentence wrapped around another.
    assert_eq!(
        err,
        "This file can't be read as a video: its data is invalid or cut short."
    );
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
    // Roots are watched with the scan filters (sizes in decimal megabytes),
    // and watched again when those settings change.
    let last_call = |fake: &FakeToolkit| fake.watch_calls.lock().unwrap().last().cloned().unwrap();
    let (called_root, patterns, min) = last_call(&app.fake);
    assert_eq!(called_root, root);
    assert_eq!(patterns, Settings::default().ignore_patterns);
    assert_eq!(min, 0);
    app.patch(
        "/api/settings",
        json!({ "min_file_size_mb": 50, "ignore_patterns": ["**/Extras/**"] }),
    )
    .await;
    let (_, patterns, min) = last_call(&app.fake);
    assert_eq!(patterns, ["**/Extras/**"]);
    assert_eq!(min, 50_000_000);
    let calls = app.fake.watch_calls.lock().unwrap().len();
    app.patch("/api/settings", json!({ "auto_queue": true }))
        .await;
    assert_eq!(app.fake.watch_calls.lock().unwrap().len(), calls);

    app.patch("/api/settings", json!({ "watch_folders": false }))
        .await;
    let fake = app.fake.clone();
    wait_until("watcher dropped", move || {
        let fake = fake.clone();
        async move { fake.watched.lock().unwrap().is_empty() }
    })
    .await;
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

/// Leftovers of interrupted conversions in a library: a temp file left in a
/// folder renamed while its file was converted is removed by the next scan,
/// and an original a crash left moved aside is put back and stays listed.
/// The files of a job that is running are left alone.
#[tokio::test]
async fn scans_clean_up_leftovers_of_interrupted_conversions() {
    use szalinski_core::paths::{backup_file_name, temp_file_name};
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/Show (2020)/a.mkv", h264());
    app.write("Movies/b.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    let movies = std::fs::canonicalize(app.media.join("Movies")).unwrap();
    let stray = movies
        .join("Show (2020)")
        .join(temp_file_name("a", uuid::Uuid::new_v4(), "mkv"));
    std::fs::write(&stray, "most of an encode").unwrap();
    let b = movies.join("b.mkv");
    let backup = movies.join(backup_file_name("b.mkv", uuid::Uuid::new_v4()));
    std::fs::rename(&b, &backup).unwrap();

    app.rescan(&id).await;
    assert!(!stray.exists(), "the stray temp file was removed");
    assert!(b.exists(), "the original was put back");
    assert!(!backup.exists());
    let files = app.files_by_name(&id).await;
    assert!(files.contains_key("b.mkv"), "{files:#?}");
    let act = app.get("/api/activity").await;
    assert!(act.text.contains("Restored"), "{}", act.text);
    assert!(!crate::state::lock(&app.state.leftovers).due());

    // A running job's temp file is its own.
    app.fake.set_behavior("b.mkv", Behavior::Hold);
    app.post(
        "/api/files/bulk",
        json!({ "action": "queue", "library": id }),
    )
    .await;
    app.resume().await;
    let state = app.state.clone();
    wait_until("the job runs", move || {
        let state = state.clone();
        async move { state.dispatcher.running_count() == 1 }
    })
    .await;
    let job_id = app.state.dispatcher.running_ids()[0];
    let own = movies.join(temp_file_name("b", job_id, "mkv"));
    std::fs::write(&own, "being written").unwrap();
    app.rescan(&id).await;
    assert!(own.exists(), "a running job's temp file was left alone");
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;
}

/// A drive mounted inside a library that is unmounted leaves an empty
/// folder behind. Its files used to be removed from the list, "Skipped by
/// you" included, and converted as new ones once the drive was back (even
/// after a restart). They stay listed, with their state, now.
#[tokio::test]
async fn files_of_an_unmounted_drive_inside_a_library_are_kept() {
    use crate::services::library::mounts::fake;
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/a.mkv", h264());
    app.write("Movies/USB/keep_me.mkv", h264());
    let usb = std::fs::canonicalize(app.media.join("Movies/USB")).unwrap();
    fake::mount(&usb);
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    let files = app.files_by_name(&id).await;
    let keep = files["keep_me.mkv"]["id"].as_str().unwrap().to_string();
    let r = app.post_empty(&format!("/api/files/{keep}/skip")).await;
    assert_eq!(r.status, axum::http::StatusCode::OK, "{}", r.text);

    // Unmounted: the folder is left empty.
    fake::unmount(&usb);
    let away = app.dir.path().join("usb-away.mkv");
    std::fs::rename(usb.join("keep_me.mkv"), &away).unwrap();
    app.rescan(&id).await;
    let files = app.files_by_name(&id).await;
    assert_eq!(files["keep_me.mkv"]["status"], "skipped", "{files:#?}");
    let act = app.get("/api/activity").await;
    assert!(act.text.contains("isn't connected"), "{}", act.text);

    // Still known after a restart; the drive comes back.
    let dir = app.stop().await;
    let app = TestApp::start(TestOptions {
        dir: Some(dir),
        ..TestOptions::default()
    })
    .await;
    app.pause().await;
    // The start-up scan.
    let fake_c = app.fake.clone();
    wait_until("the catch-up scan", move || {
        let fake = fake_c.clone();
        async move { !fake.walks.lock().unwrap().is_empty() }
    })
    .await;
    app.wait_scan(&id).await;
    app.rescan(&id).await;
    assert_eq!(
        app.files_by_name(&id).await["keep_me.mkv"]["status"],
        "skipped"
    );
    std::fs::rename(&away, usb.join("keep_me.mkv")).unwrap();
    fake::mount(&usb);
    app.rescan(&id).await;
    let files = app.files_by_name(&id).await;
    assert_eq!(files["keep_me.mkv"]["status"], "skipped", "{files:#?}");
    let queued = app.get("/api/jobs?state=queued").await;
    assert!(!queued.text.contains("keep_me.mkv"), "{}", queued.text);

    // A folder that was never a mount loses its files as before.
    fake::unmount(&usb);
    std::fs::remove_file(app.media.join("Movies/a.mkv")).unwrap();
    app.rescan(&id).await;
    assert!(!app.files_by_name(&id).await.contains_key("a.mkv"));
}

/// A folder renamed while one of its files was being converted: the
/// converter goes on writing into the folder's new place and then finds no
/// new file at the old path. The temp file left there is removed once the
/// job ends, even with folder watching off; other jobs' files stay.
#[tokio::test]
async fn a_moved_files_temp_file_is_removed_when_its_job_ends() {
    use szalinski_core::paths::temp_file_name;
    let app = TestApp::new().await;
    app.pause().await;
    let r = app
        .patch("/api/settings", json!({ "watch_folders": false }))
        .await;
    assert_eq!(r.status, axum::http::StatusCode::OK, "{}", r.text);
    app.write("Movies/Show/ep.mkv", h264());
    app.write("Movies/other.mkv", h264());
    app.add_library("Movies", json!({})).await;
    // The job is queued by the scan, which a busy machine may still be
    // finishing up (wait for it rather than assume it).
    let queued_ep = || async {
        app.get("/api/jobs?state=queued").await.json["items"]
            .as_array()
            .and_then(|jobs| jobs.iter().find(|j| j["file_name"] == "ep.mkv").cloned())
    };
    wait_until("the job to be queued", || async {
        queued_ep().await.is_some()
    })
    .await;
    let job = queued_ep().await.unwrap();
    let job_id: uuid::Uuid = job["id"].as_str().unwrap().parse().unwrap();
    let movies = std::fs::canonicalize(app.media.join("Movies")).unwrap();
    app.fake.set_behavior(
        "ep.mkv",
        Behavior::HoldFail("finished without writing a new file".into()),
    );
    app.resume().await;
    let state = app.state.clone();
    wait_until("the job runs", move || {
        let state = state.clone();
        async move { state.dispatcher.running_ids().contains(&job_id) }
    })
    .await;
    // Renamed during the encode: the temp file is in the folder's new place.
    std::fs::rename(movies.join("Show"), movies.join("Show (2020)")).unwrap();
    let stray = movies
        .join("Show (2020)")
        .join(temp_file_name("ep", job_id, "mkv"));
    std::fs::write(&stray, "most of an encode").unwrap();
    let other = movies
        .join("Show (2020)")
        .join(temp_file_name("ep", uuid::Uuid::new_v4(), "mkv"));
    std::fs::write(&other, "another job's").unwrap();
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;
    // The library is searched once the job has ended, in the background
    // (generous for a busy machine).
    wait_until_for(
        "the stray temp file is removed",
        Duration::from_secs(60),
        || async { !stray.exists() },
    )
    .await;
    assert!(other.exists(), "only that job's files");
    let j = app.get(&format!("/api/jobs/{job_id}")).await;
    assert_eq!(j.json["problem"], "source_changed", "{}", j.json);
}

/// The share starts answering with errors between a scan's walk and its
/// probe of a new file (here a file stands where the file's folder was):
/// that says nothing about the file, so it is left for the next scan
/// instead of being recorded as a file that can't be read. Once the share
/// works again, the next scan lists it as usual.
#[tokio::test]
async fn a_file_its_share_answers_errors_about_is_left_for_the_next_scan() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/old.mkv", h264());
    let lib = app.add_library("Movies", json!({})).await;
    let id = lib["id"].as_str().unwrap().to_string();
    app.write("Movies/Film/film.mkv", h264());
    let folder = app.media.join("Movies/Film");
    let away = app.media.join("Movies/Film.away");

    app.fake.walk_gate.close();
    let r = app.post_empty(&format!("/api/libraries/{id}/scan")).await;
    assert_eq!(r.status, axum::http::StatusCode::ACCEPTED, "{}", r.text);
    app.fake.walk_gate.wait_reached().await;
    std::fs::rename(&folder, &away).unwrap();
    std::fs::write(&folder, "not a folder").unwrap();
    app.fake.walk_gate.release();
    app.wait_scan(&id).await;
    let failed = app.get("/api/files?status=failed").await.json;
    assert_eq!(failed["total"], 0, "{failed}");
    let files = app.files_by_name(&id).await;
    assert!(!files.contains_key("film.mkv"), "{files:#?}");

    std::fs::remove_file(&folder).unwrap();
    std::fs::rename(&away, &folder).unwrap();
    app.rescan(&id).await;
    let files = app.files_by_name(&id).await;
    let film = &files["film.mkv"];
    assert_ne!(film["status"], "failed", "{film:#}");
}
