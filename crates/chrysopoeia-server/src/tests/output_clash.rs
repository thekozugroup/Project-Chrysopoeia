//! Two files converted to one name: with "save converted files to a
//! separate folder", every library's folders are mirrored in the one output
//! folder without the library's name, so the same relative path in two
//! libraries gives one output file. The second is refused (never written
//! over the first), and its message says whose file has the name and what
//! to do.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::support::*;

const FROZEN: &str = "Movies/Frozen (2013)/Frozen.mkv";

/// Folder mode into `out`, at most `max_jobs` files at once, the queue
/// paused, and the fake refusing destinations already taken (as the worker
/// does).
async fn folder_mode(app: &TestApp, max_jobs: u32) -> PathBuf {
    let out = app.dir.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let r = app
        .patch(
            "/api/settings",
            json!({
                "output_mode": "folder",
                "output_folder": out.to_str().unwrap(),
                "max_jobs": max_jobs,
            }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.fake.check_destination.store(true, Ordering::SeqCst);
    app.pause().await;
    out
}

/// A fake video whose converted file still says where it came from.
fn source(from: &str) -> String {
    format!("source={from}\n{}", h264())
}

/// Two libraries, Movies and Kids, both holding [`FROZEN`].
async fn two_libraries(app: &TestApp) -> (String, String) {
    app.write(&format!("movies/{FROZEN}"), &source("movies"));
    app.write(&format!("kids/{FROZEN}"), &source("kids"));
    let movies = app.add_library("movies", json!({ "name": "Movies" })).await;
    let kids = app.add_library("kids", json!({ "name": "Kids" })).await;
    (
        movies["id"].as_str().unwrap().to_string(),
        kids["id"].as_str().unwrap().to_string(),
    )
}

async fn frozen(app: &TestApp, lib: &str) -> Value {
    app.files_by_name(lib).await["Frozen.mkv"].clone()
}

fn starts_with(path: &Path, text: &str) -> bool {
    std::fs::read_to_string(path).is_ok_and(|c| c.starts_with(text))
}

/// One at a time: the second library's job is refused before anything is
/// encoded, and says which library's converted file has the name.
#[tokio::test]
async fn the_second_library_is_told_whose_converted_file_has_the_name() {
    let app = TestApp::new().await;
    let out = folder_mode(&app, 1).await;
    let (movies, kids) = two_libraries(&app).await;
    app.resume().await;
    app.wait_queue_idle().await;

    let first = frozen(&app, &movies).await;
    let second = frozen(&app, &kids).await;
    assert_eq!(first["status"], "done", "{first}");
    assert_eq!(second["status"], "failed", "{second}");
    assert_eq!(second["problem"], "destination");
    assert_eq!(
        second["error"],
        format!(
            "Another library's converted file, from Movies, already uses the name \
             \"Movies/Frozen (2013)/Frozen.mkv\" in the output folder {}, so this file wasn't \
             converted and that file wasn't overwritten. Rename one of the two files, or \
             convert one library at a time, each to a different output folder chosen in \
             Settings › Output.",
            out.display()
        )
    );
    // The first library's result is untouched, and so are both originals.
    assert!(starts_with(&out.join(FROZEN), "source=movies"));
    assert!(starts_with(
        &app.media.join("kids").join(FROZEN),
        "source=kids"
    ));
    assert!(starts_with(
        &app.media.join("movies").join(FROZEN),
        "source=movies"
    ));
    let feed = app.get("/api/activity").await.json["items"].to_string();
    assert!(
        feed.contains("Failed Frozen.mkv: Another library's converted file, from Movies"),
        "{feed}"
    );
}

/// Two at once ("Files at once" = 2): the second to start is refused at
/// once, while the first is still converting, so it never encodes (and
/// can't overwrite the first's file when both finish).
#[tokio::test]
async fn two_libraries_converting_to_one_name_at_once_never_overwrite() {
    let app = TestApp::new().await;
    let out = folder_mode(&app, 2).await;
    app.fake.set_default(Behavior::Hold);
    let (movies, kids) = two_libraries(&app).await;
    app.resume().await;
    wait_until("one job to be refused", || async {
        app.get("/api/files?status=failed").await.json["total"] == 1
    })
    .await;
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;

    let files = [
        ("Movies", "movies", frozen(&app, &movies).await),
        ("Kids", "kids", frozen(&app, &kids).await),
    ];
    let (done, refused): (Vec<_>, Vec<_>) =
        files.iter().partition(|(_, _, f)| f["status"] == "done");
    assert_eq!((done.len(), refused.len()), (1, 1), "{files:?}");
    let (winner, winner_dir, _) = done[0];
    let (_, _, loser) = refused[0];
    assert_eq!(loser["problem"], "destination");
    assert_eq!(
        loser["error"],
        format!(
            "Another library's file, from {winner}, is being converted to the same name, \
             \"Movies/Frozen (2013)/Frozen.mkv\", in the output folder {}, so this file wasn't \
             converted. Rename one of the two files, or convert one library at a time, each to \
             a different output folder chosen in Settings › Output.",
            out.display()
        )
    );
    // Only the winner was ever encoded, and its result is in place.
    assert_eq!(app.fake.started(), ["Frozen.mkv"]);
    assert!(starts_with(
        &out.join(FROZEN),
        &format!("source={winner_dir}")
    ));
}

/// Two originals of one library that differ only by extension become one
/// name too: the message names the other file.
#[tokio::test]
async fn two_files_of_one_library_with_one_output_name_are_told_apart() {
    let app = TestApp::new().await;
    let out = folder_mode(&app, 1).await;
    app.write("Movies/Movie.avi", &source("avi"));
    app.write("Movies/Movie.mp4", &source("mp4"));
    let lib = app.add_library("Movies", json!({})).await;
    let lib = lib["id"].as_str().unwrap().to_string();
    app.resume().await;
    app.wait_queue_idle().await;

    let files = app.files_by_name(&lib).await;
    let (done, failed) = if files["Movie.avi"]["status"] == "done" {
        ("Movie.avi", "Movie.mp4")
    } else {
        ("Movie.mp4", "Movie.avi")
    };
    assert_eq!(files[done]["status"], "done", "{files:?}");
    assert_eq!(files[failed]["status"], "failed");
    assert_eq!(
        files[failed]["error"],
        format!(
            "Another file in this library, \"{done}\", was converted to the same name, \
             \"Movie.mkv\", in the output folder {}, so this file wasn't converted and that \
             file wasn't overwritten. Rename one of the two files, then convert this one again.",
            out.display()
        )
    );
}

/// Replacing originals: two files in one folder that differ only by
/// extension, converted at once, can't both take the new name.
#[tokio::test]
async fn two_originals_replaced_by_one_name_at_once_are_told_apart() {
    let app = TestApp::new().await;
    app.pause().await;
    app.fake.set_default(Behavior::Hold);
    app.write("Movies/Movie.avi", &source("avi"));
    app.write("Movies/Movie.mp4", &source("mp4"));
    let lib = app.add_library("Movies", json!({})).await;
    let lib = lib["id"].as_str().unwrap().to_string();
    app.resume().await;
    wait_until("one job to be refused", || async {
        app.get("/api/files?status=failed").await.json["total"] == 1
    })
    .await;
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;

    let files = app.files_by_name(&lib).await;
    let failed = files
        .values()
        .find(|f| f["status"] == "failed")
        .unwrap_or_else(|| panic!("{files:?}"));
    let other = if failed["file_name"] == "Movie.avi" {
        "Movie.mp4"
    } else {
        "Movie.avi"
    };
    assert_eq!(
        failed["error"],
        format!(
            "Another file in the same folder, \"{other}\", is being converted to the same \
             name, \"Movie.mkv\", so this file wasn't converted. Rename one of the two files, \
             then convert this one again."
        )
    );
    assert_eq!(app.fake.started().len(), 1);
    assert_eq!(files["Movie.mkv"]["status"], "done", "{files:?}");
}
