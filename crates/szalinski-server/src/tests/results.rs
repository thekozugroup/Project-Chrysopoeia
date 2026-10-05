//! Round 5: what a finished conversion says about itself (the space it
//! freed, the name of its result), finding a converted file by the name it
//! had before, failure entries in the feed that carry their kind, the folder
//! watcher not taking Szalinski's own results for copies in progress, and
//! plain wording for bodies the API can't use.

use std::collections::HashMap;
use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use szalinski_core::{ActivityLevel, Event, JobState, ProblemKind};
use tower::ServiceExt as _;

use super::support::*;
use crate::db;
use crate::db::activity::ActivityRefs;

fn id_of(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

/// A body big enough that a conversion has something to save.
fn padded() -> String {
    format!("{}{}", h264(), "p".repeat(900))
}

/// The jobs of the history, keyed by the name of the file they were queued
/// with.
async fn history_by_name(app: &TestApp) -> HashMap<String, Value> {
    let r = app.get("/api/jobs?state=history&limit=100").await;
    r.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| (j["file_name"].as_str().unwrap().to_string(), j.clone()))
        .collect()
}

/// The names of the files a search finds.
async fn found_by(app: &TestApp, query: &str) -> Vec<String> {
    let r = app.get(&format!("/api/files?q={query}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.json["total"].as_u64().unwrap() as usize,
        r.json["items"].as_array().unwrap().len(),
        "{}",
        r.text
    );
    let mut names: Vec<String> = r.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["file_name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn a_conversion_that_renames_reports_its_result_and_its_old_name_still_finds_it() {
    let app = TestApp::new().await;
    let mut events = app.state.events.subscribe();
    app.write("Movies/land1080.mp4", &padded());
    app.write("Movies/same.mkv", &padded());
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib_id = id_of(&lib);
    app.wait_queue_idle().await;

    let files = app.files_by_name(&lib_id).await;
    assert_eq!(files["land1080.mkv"]["status"], "done", "{files:?}");
    assert_eq!(files["same.mkv"]["status"], "done");
    assert!(!files.contains_key("land1080.mp4"));

    // The job says what it freed and what its result is called.
    let jobs = history_by_name(&app).await;
    let renamed = &jobs["land1080.mp4"];
    assert_eq!(renamed["state"], "done", "{renamed}");
    assert_eq!(renamed["output_name"], "land1080.mkv", "{renamed}");
    let freed = renamed["input_size"].as_i64().unwrap() - renamed["output_size"].as_i64().unwrap();
    assert!(freed > 0, "{renamed}");
    assert_eq!(renamed["freed_bytes"], freed, "{renamed}");
    // The file's own saving is the same number.
    assert_eq!(files["land1080.mkv"]["saved_bytes"], freed);
    // A result with the original's name has no other name to give.
    let same = &jobs["same.mkv"];
    assert!(same["output_name"].is_null(), "{same}");
    assert!(same["freed_bytes"].as_i64().unwrap() > 0, "{same}");
    // The same fields come with a job read by itself and on the file's page.
    let one = app.get(&format!("/api/jobs/{}", id_of(renamed))).await.json;
    assert_eq!(one["output_name"], "land1080.mkv");
    assert_eq!(one["freed_bytes"], freed);
    let detail = app
        .get(&format!("/api/files/{}", id_of(&files["land1080.mkv"])))
        .await
        .json;
    assert_eq!(detail["jobs"][0]["output_name"], "land1080.mkv");

    // And in the events the UI updates from.
    let mut seen = None;
    loop {
        let event = match events.try_recv() {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        };
        if let Event::JobUpdated { job } = event
            && job.state == JobState::Done
            && job.file_name == "land1080.mp4"
        {
            seen = Some(job);
        }
    }
    let job = seen.expect("a job.updated for the finished conversion");
    assert_eq!(job.output_name.as_deref(), Some("land1080.mkv"));
    assert_eq!(job.freed_bytes, Some(freed as u64));

    // The old name finds the file, whatever the case, alongside the new one.
    assert_eq!(found_by(&app, "land1080.mp4").await, ["land1080.mkv"]);
    assert_eq!(found_by(&app, "LAND1080.MP4").await, ["land1080.mkv"]);
    assert_eq!(found_by(&app, "land1080.mkv").await, ["land1080.mkv"]);
    assert_eq!(found_by(&app, "land1080.m").await, ["land1080.mkv"]);
    assert!(found_by(&app, "land1080.avi").await.is_empty());
    assert!(found_by(&app, "same.mp4").await.is_empty());
    // The wildcard characters of a search stay plain text.
    assert!(found_by(&app, "land%25.mp4").await.is_empty());
    // With the other filters.
    let r = app
        .get(&format!(
            "/api/files?q=land1080.mp4&library={lib_id}&status=done"
        ))
        .await;
    assert_eq!(r.json["total"], 1, "{}", r.text);
    let r = app.get("/api/files?q=land1080.mp4&status=failed").await;
    assert_eq!(r.json["total"], 0, "{}", r.text);

    // Converted again: two jobs, still one row.
    let id = id_of(&files["land1080.mkv"]);
    let r = app
        .post(&format!("/api/files/{id}/queue"), json!({ "force": true }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.wait_queue_idle().await;
    assert_eq!(found_by(&app, "land1080.mp4").await, ["land1080.mkv"]);
    let r = app.get("/api/jobs?state=history&limit=100").await;
    let of_file: Vec<&Value> = r.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|j| j["file_id"] == json!(id))
        .collect();
    assert_eq!(of_file.len(), 2, "{}", r.text);
    // The second conversion started from the new name and kept it.
    assert!(
        of_file
            .iter()
            .any(|j| j["file_name"] == "land1080.mkv" && j["output_name"].is_null()),
        "{of_file:?}"
    );
}

/// Replacing an original that has another hard link (a seeding torrent's
/// copy) frees nothing; neither does a result that isn't smaller.
#[cfg(unix)]
#[tokio::test]
async fn a_conversion_that_released_no_space_says_so() {
    let app = TestApp::new().await;
    app.pause().await;
    let file = app.write("Movies/Linked (2020).mp4", &padded());
    std::fs::hard_link(&file, app.media.join("torrent-copy.mp4")).unwrap();
    // What the worker adds to the notes of such a conversion.
    *app.fake.done_notes.lock().unwrap() =
        vec![szalinski_worker::run::SHARED_ORIGINAL_NOTE.to_string()];
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib_id = id_of(&lib);
    app.resume().await;
    app.wait_queue_idle().await;
    *app.fake.done_notes.lock().unwrap() = Vec::new();

    let files = app.files_by_name(&lib_id).await;
    assert_eq!(files["Linked (2020).mkv"]["saved_bytes"], 0);
    let jobs = history_by_name(&app).await;
    let linked = &jobs["Linked (2020).mp4"];
    assert_eq!(linked["state"], "done", "{linked}");
    assert!(
        linked["output_size"].as_i64().unwrap() < linked["input_size"].as_i64().unwrap(),
        "the result is smaller, but the space wasn't released: {linked}"
    );
    assert_eq!(linked["freed_bytes"], 0, "{linked}");
    assert_eq!(linked["output_name"], "Linked (2020).mkv", "{linked}");

    // A result that came out larger frees nothing either (never below 0).
    app.write("Movies/Bigger.mkv", &padded());
    app.fake
        .set_behavior("Bigger.mkv", Behavior::Done { ratio: 1.5 });
    app.rescan(&lib_id).await;
    app.wait_queue_idle().await;
    let jobs = history_by_name(&app).await;
    let bigger = &jobs["Bigger.mkv"];
    assert_eq!(bigger["state"], "done", "{bigger}");
    assert!(
        bigger["output_size"].as_i64().unwrap() > bigger["input_size"].as_i64().unwrap(),
        "{bigger}"
    );
    assert_eq!(bigger["freed_bytes"], 0, "{bigger}");
}

/// Only a conversion that is done has a freed amount or a new name.
#[tokio::test]
async fn jobs_that_are_not_done_have_neither_freed_bytes_nor_a_result_name() {
    let app = TestApp::new().await;
    app.pause().await;
    app.write("Movies/queued.mp4", &padded());
    app.write("Movies/failed.mp4", &padded());
    app.write("Movies/skipped.mp4", &padded());
    app.fake.set_behavior(
        "failed.mp4",
        Behavior::FailWith(ProblemKind::Encoder, "The encoder stopped.".into()),
    );
    app.fake
        .set_behavior("skipped.mp4", Behavior::Skip("Not worth it".into()));
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib_id = id_of(&lib);
    let files = app.files_by_name(&lib_id).await;
    // Only the first is left waiting.
    app.post_empty(&format!("/api/files/{}/skip", id_of(&files["queued.mp4"])))
        .await;
    let queued = app.get("/api/jobs?state=queued").await;
    for job in queued.json["items"].as_array().unwrap() {
        assert!(job["freed_bytes"].is_null(), "{job}");
        assert!(job["output_name"].is_null(), "{job}");
    }
    app.resume().await;
    app.wait_queue_idle().await;
    let jobs = history_by_name(&app).await;
    for name in ["failed.mp4", "skipped.mp4"] {
        let job = &jobs[name];
        assert!(job["freed_bytes"].is_null(), "{job}");
        assert!(job["output_name"].is_null(), "{job}");
    }
}

/// An entry about a failed file carries the kind of problem (as the job
/// does), in the feed, after a restart of the read and in the event.
#[tokio::test]
async fn a_failure_in_the_feed_carries_its_kind_of_problem() {
    let app = TestApp::new().await;
    let mut events = app.state.events.subscribe();
    app.write("Movies/broken.mkv", h264());
    app.write("Movies/fine.mkv", h264());
    app.write("Movies/skipme.mkv", h264());
    app.fake.set_behavior(
        "broken.mkv",
        Behavior::FailWith(ProblemKind::DiskFull, "The disk is full.".into()),
    );
    app.fake
        .set_behavior("skipme.mkv", Behavior::Skip("Not worth it".into()));
    let lib = app.add_library("Movies", json!({})).await;
    let lib_id = id_of(&lib);
    app.wait_queue_idle().await;

    let feed = app.get("/api/activity?limit=100").await.json;
    let entries = feed["items"].as_array().unwrap();
    let entry = |prefix: &str| -> &Value {
        entries
            .iter()
            .find(|e| e["message"].as_str().unwrap().starts_with(prefix))
            .unwrap_or_else(|| panic!("no entry starting with {prefix:?}: {feed}"))
    };
    assert_eq!(entry("Failed broken.mkv")["problem"], "disk_full");
    assert_eq!(entry("Failed broken.mkv")["level"], "error");
    assert!(entry("Converted fine.mkv")["problem"].is_null());
    assert!(entry("Skipped skipme.mkv")["problem"].is_null());
    // Entries about a library as a whole have none.
    assert!(
        entries
            .iter()
            .filter(|e| e["job_id"].is_null() && e["file_id"].is_null())
            .all(|e| e["problem"].is_null()),
        "{feed}"
    );

    // The event the UI prepends to the feed has it too.
    let mut carried = Vec::new();
    loop {
        let event = match events.try_recv() {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        };
        if let Event::Activity { entry } = event {
            carried.push((entry.message, entry.problem));
        }
    }
    assert!(
        carried
            .iter()
            .any(|(m, p)| m.starts_with("Failed broken.mkv") && *p == Some(ProblemKind::DiskFull)),
        "{carried:?}"
    );
    assert!(
        carried
            .iter()
            .filter(|(m, _)| !m.starts_with("Failed"))
            .all(|(_, p)| p.is_none()),
        "{carried:?}"
    );

    // An entry about a failed file that has no job (a file found by the
    // watcher that can't be read, say) takes the file's kind; the same
    // entry about a file that is fine, or a success entry, has none.
    let files = app.files_by_name(&lib_id).await;
    let pool = app.state.db.pool();
    let broken = id_of(&files["broken.mkv"]).parse().unwrap();
    let fine = id_of(&files["fine.mkv"]).parse().unwrap();
    let about = |file| ActivityRefs {
        file_id: Some(file),
        ..ActivityRefs::default()
    };
    let e = db::activity::insert(pool, ActivityLevel::Info, "Found it", about(broken))
        .await
        .unwrap();
    assert_eq!(e.problem, Some(ProblemKind::DiskFull));
    let e = db::activity::insert(pool, ActivityLevel::Info, "Found it", about(fine))
        .await
        .unwrap();
    assert_eq!(e.problem, None);
    let e = db::activity::insert(pool, ActivityLevel::Success, "Fixed", about(broken))
        .await
        .unwrap();
    assert_eq!(e.problem, None);
    // Stored: a later read still has it.
    let stored = db::activity::list(pool, 100, None).await.unwrap();
    assert!(
        stored
            .iter()
            .any(|e| e.message.starts_with("Failed broken.mkv")
                && e.problem == Some(ProblemKind::DiskFull)),
        "{stored:?}"
    );
}

/// The folder watcher's count of files still being copied leaves out what
/// Szalinski itself writes: the result of a running conversion and, for a
/// while after, the result of a finished one.
#[tokio::test]
async fn the_watcher_does_not_count_szalinskis_own_results_as_copies() {
    let app = TestApp::new().await;
    app.write("Movies/held.mkv", h264());
    app.fake.set_behavior("held.mkv", Behavior::Hold);
    let lib = app.add_library("Movies", json!({})).await;
    let root = PathBuf::from(lib["path"].as_str().unwrap());
    let held = root.join("held.mkv");
    let copying = root.join("copying.mkv");
    wait_until("the conversion to start", || async {
        app.fake.started() == ["held.mkv"]
    })
    .await;

    let waiting = |files: &[&PathBuf]| {
        HashMap::from([(root.clone(), files.iter().map(|p| (*p).clone()).collect())])
    };
    let count = |files: HashMap<PathBuf, Vec<PathBuf>>| {
        let state = app.state.clone();
        async move {
            crate::services::library::settling_copies(&state, &files)
                .await
                .unwrap()
        }
    };
    // While it runs: its file isn't a copy, another one is.
    assert_eq!(
        count(waiting(&[&held, &copying])).await,
        HashMap::from([(root.clone(), 1)])
    );
    assert!(count(waiting(&[&held])).await.is_empty());
    assert!(count(HashMap::new()).await.is_empty());
    assert!(
        count(HashMap::from([(root.clone(), vec![])]))
            .await
            .is_empty()
    );

    // Done a moment ago: still its own (the watcher only reports the file
    // after its settle time).
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;
    assert!(count(waiting(&[&held])).await.is_empty());
    assert_eq!(
        count(waiting(&[&held, &copying])).await,
        HashMap::from([(root.clone(), 1)])
    );

    // Long ago: a file that changes at that name is a copy like any other.
    sqlx::query("UPDATE jobs SET finished_at = '2020-01-01T00:00:00.000Z'")
        .execute(app.state.db.pool())
        .await
        .unwrap();
    assert_eq!(
        count(waiting(&[&held, &copying])).await,
        HashMap::from([(root.clone(), 2)])
    );
}

/// A result a conversion is still putting in place is recorded by the job
/// when it ends: a watch event for it is not probed, and under a new name
/// it doesn't become a second row for the file.
#[tokio::test]
async fn a_watch_event_for_a_result_still_being_put_in_place_is_left_to_the_job() {
    use std::sync::atomic::Ordering;
    let app = TestApp::new().await;
    app.write("Movies/land.mp4", &padded());
    app.fake.set_behavior("land.mp4", Behavior::Hold);
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib_id = id_of(&lib);
    let root = PathBuf::from(lib["path"].as_str().unwrap());
    wait_until("the conversion to start", || async {
        app.fake.started() == ["land.mp4"]
    })
    .await;

    // The result appears under its new name, and the watcher reports it.
    std::fs::write(root.join("land.mkv"), padded()).unwrap();
    let probes = app.fake.probes.load(Ordering::SeqCst);
    let tx = app.fake.watch_sender();
    tx.send(szalinski_scanner::WatchEvent::Upserted(
        root.join("land.mkv"),
    ))
    .await
    .unwrap();
    // Events are handled in order: once the one after it is stored, this
    // one has been handled.
    app.write("Movies/other.mkv", h264());
    // (Kept as it is, so nothing else is probed on the way.)
    app.fake
        .set_behavior("other.mkv", Behavior::Skip("Not worth it".into()));
    tx.send(szalinski_scanner::WatchEvent::Upserted(
        root.join("other.mkv"),
    ))
    .await
    .unwrap();
    wait_until("the next file to be stored", || async {
        app.files_by_name(&lib_id).await.contains_key("other.mkv")
    })
    .await;
    let files = app.files_by_name(&lib_id).await;
    assert!(
        !files.contains_key("land.mkv"),
        "the result became a file of its own: {files:?}"
    );
    assert_eq!(
        app.fake.probes.load(Ordering::SeqCst),
        probes + 1,
        "only the other file was probed"
    );

    // The job ends and records its result under the new name: one row.
    app.fake.release.add_permits(1);
    wait_until("the conversion to finish", || async {
        app.files_by_name(&lib_id)
            .await
            .get("land.mkv")
            .is_some_and(|f| f["status"] == "done")
    })
    .await;
    let files = app.files_by_name(&lib_id).await;
    assert!(!files.contains_key("land.mp4"), "{files:?}");
    let feed = app.get("/api/activity?limit=100").await.text;
    assert!(!feed.contains("Found land.mkv"), "{feed}");
}

/// What a request body the API can't use is told: in plain words, naming
/// the value when it is known, with nothing of the parser's own text and no
/// internal type name.
#[tokio::test]
async fn bodies_the_api_cannot_use_are_explained_in_plain_words() {
    let app = TestApp::new().await;
    let send = |uri: &'static str, body: &'static str| {
        let router = app.router.clone();
        async move {
            let req = Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap();
            let resp = router.oneshot(req).await.unwrap();
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
                .await
                .unwrap();
            (status, serde_json::from_slice::<Value>(&bytes).unwrap())
        }
    };
    let file = "/api/files/00000000-0000-0000-0000-000000000001/queue";
    const OBJECT: &str =
        "The request body should be an object with named values, like {\"name\": \"value\"}.";
    const NOT_JSON: &str = "The request body isn't valid JSON. It may be cut off, or have a \
                            missing quote, bracket or comma.";
    // (route, body, code, message, field)
    let cases: [(&str, &str, &str, &str, Option<&str>); 13] = [
        ("/api/libraries", "", "invalid_json", NOT_JSON, None),
        ("/api/libraries", "{", "invalid_json", NOT_JSON, None),
        (
            "/api/libraries",
            "{\"path\": ",
            "invalid_json",
            NOT_JSON,
            None,
        ),
        ("/api/libraries", "nope", "invalid_json", NOT_JSON, None),
        (
            "/api/libraries",
            "{}",
            "invalid_request",
            "The request is missing \"path\".",
            None,
        ),
        ("/api/libraries", "\"x\"", "invalid_request", OBJECT, None),
        ("/api/libraries", "[1, 2]", "invalid_request", OBJECT, None),
        ("/api/libraries", "7", "invalid_request", OBJECT, None),
        (
            "/api/libraries",
            "{\"path\": 5}",
            "invalid_request",
            "The value for \"path\" isn't valid. It must be text.",
            Some("path"),
        ),
        (
            "/api/libraries",
            "{\"path\": \"/x\", \"name\": 7}",
            "invalid_request",
            "The value for \"name\" isn't valid. It must be text.",
            Some("name"),
        ),
        // The same for a body that may be left out.
        (file, "{\"force\": tru", "invalid_json", NOT_JSON, None),
        (
            file,
            "{\"force\": \"yes\"}",
            "invalid_request",
            "The value for \"force\" isn't valid. It must be true or false.",
            Some("force"),
        ),
        (file, "{\"force\": true} x", "invalid_json", NOT_JSON, None),
    ];
    for (uri, body, code, message, field) in cases {
        let (status, json) = send(uri, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}: {json}");
        assert_eq!(json["code"], code, "{body:?}: {json}");
        assert_eq!(json["error"], message, "{body:?}: {json}");
        assert_eq!(json["field"], json!(field), "{body:?}: {json}");
        let text = json["error"].as_str().unwrap();
        for jargon in [
            "EOF", "expected", "struct", "Body", "`", " at line", "column", "serde", "Option",
        ] {
            assert!(!text.contains(jargon), "{body:?} says {text:?}");
        }
    }
}

/// The names of libraries in the messages about them are quoted, as the web
/// quotes them.
#[tokio::test]
async fn library_names_are_quoted_in_messages() {
    let app = TestApp::new().await;
    app.add_library("Movies/Compat", json!({})).await;
    let root = app.media.join("Movies/Compat");
    let attempt = |rel: &str| {
        let path = app.media.join(rel);
        std::fs::create_dir_all(&path).unwrap();
        let app = &app;
        async move {
            let r = app
                .post("/api/libraries", json!({ "path": path.to_str().unwrap() }))
                .await;
            assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
            (
                r.json["code"].as_str().unwrap().to_string(),
                r.json["error"].as_str().unwrap().to_string(),
            )
        }
    };
    assert!(root.exists());
    assert_eq!(
        attempt("Movies/Compat").await,
        (
            "library_exists".to_string(),
            "That folder is already the library “Compat”.".to_string()
        )
    );
    assert_eq!(
        attempt("Movies/Compat/Extras").await,
        (
            "library_overlaps".to_string(),
            "That folder is inside “Compat”, which is already a library.".to_string()
        )
    );
    assert_eq!(
        attempt("Movies").await,
        (
            "library_overlaps".to_string(),
            "That folder contains the library “Compat”. Pick a different folder, or remove \
             “Compat” first."
                .to_string()
        )
    );
    // Added and removed: the feed quotes it too.
    let feed = app.get("/api/activity?limit=50").await.text;
    assert!(feed.contains("Added the library “Compat”"), "{feed}");
}
