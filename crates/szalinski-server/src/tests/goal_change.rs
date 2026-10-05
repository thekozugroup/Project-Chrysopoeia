//! A library's goal changed while one of its files was being converted: the
//! file ends with a verdict of the goal the library has now, and scans
//! decide again any verdict an earlier goal left.

use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{Value, json};
use szalinski_core::{Goal, ProblemKind, TranscodeProfile, VideoCodec};

use super::support::*;

const SIZE_RULE: &str = "Only 4% smaller — kept the original";

fn goal(goal: Goal) -> Value {
    serde_json::to_value(TranscodeProfile::from_goal(goal)).unwrap()
}

fn already(codec: VideoCodec) -> String {
    format!("Already {}", codec.label())
}

async fn set_goal(app: &TestApp, lib: &str, profile: Value) {
    let r = app
        .patch(
            &format!("/api/libraries/{lib}"),
            json!({ "profile": profile }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

/// Wait until `n` jobs run, and the fake converter has started them (so
/// what they do is settled).
async fn wait_running(app: &TestApp, n: u64) {
    wait_until("jobs to run", || async {
        app.get("/api/jobs?state=running").await.json["total"] == n
            && app.fake.started().len() as u64 >= n
    })
    .await;
}

/// The jobs of a file, oldest first.
async fn jobs_of(app: &TestApp, file: &Value) -> Vec<Value> {
    let id = file["id"].as_str().unwrap();
    let mut jobs = app.get(&format!("/api/files/{id}")).await.json["jobs"]
        .as_array()
        .unwrap()
        .clone();
    jobs.reverse();
    jobs
}

async fn feed(app: &TestApp) -> String {
    app.get("/api/activity").await.json["items"].to_string()
}

/// The acceptance report: a file converting under "Save space" (with its
/// 10% minimum) whose result was not small enough, while the goal changed to
/// "Plays everywhere" (no minimum). It used to end skipped with the old
/// goal's size-rule reason, and a rescan didn't change it. Now it ends with
/// the new goal's verdict (here: already in the new goal's format).
#[tokio::test]
async fn a_size_rule_skip_under_the_old_goal_ends_with_the_new_goals_verdict() {
    let app = TestApp::new().await;
    app.fake.set_behavior(
        "a.mkv",
        Behavior::HoldThen(Box::new(Behavior::Skip(SIZE_RULE.into()))),
    );
    app.write("Movies/a.mkv", h264());
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib = lib["id"].as_str().unwrap().to_string();
    wait_running(&app, 1).await;
    set_goal(&app, &lib, goal(Goal::Compatible)).await;
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;

    let file = app.files_by_name(&lib).await["a.mkv"].clone();
    assert_eq!(file["status"], "skipped", "{file}");
    assert_eq!(file["skip_reason"], already(VideoCodec::H264));
    // The job keeps what happened to it.
    let jobs = jobs_of(&app, &file).await;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["skip_reason"], SIZE_RULE);
    let feed = feed(&app).await;
    assert!(
        feed.contains(&format!(
            "The goal of Movies changed while a.mkv was being converted. Under the new goal it \
             is left as it is: {}.",
            already(VideoCodec::H264)
        )),
        "{feed}"
    );

    // A rescan finds nothing to change.
    app.rescan(&lib).await;
    app.wait_queue_idle().await;
    let again = app.files_by_name(&lib).await["a.mkv"].clone();
    assert_eq!(again["skip_reason"], already(VideoCodec::H264));
    assert_eq!(jobs_of(&app, &again).await.len(), 1);
}

/// The new goal would convert the file: it is queued again and converted
/// for the new goal.
#[tokio::test]
async fn a_skip_under_the_old_goal_is_converted_for_the_new_goal() {
    let app = TestApp::new().await;
    app.fake.set_behavior(
        "a.mkv",
        Behavior::HoldThen(Box::new(Behavior::Skip(SIZE_RULE.into()))),
    );
    app.write("Movies/a.mkv", h264());
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib = lib["id"].as_str().unwrap().to_string();
    wait_running(&app, 1).await;
    // The next run converts it.
    app.fake
        .set_behavior("a.mkv", Behavior::Done { ratio: 0.5 });
    set_goal(&app, &lib, goal(Goal::Balanced)).await;
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;

    let file = app.files_by_name(&lib).await["a.mkv"].clone();
    assert_eq!(file["status"], "done", "{file}");
    assert_eq!(file["video_codec"], "hevc");
    let states: Vec<Value> = jobs_of(&app, &file)
        .await
        .iter()
        .map(|j| j["state"].clone())
        .collect();
    assert_eq!(states, [json!("skipped"), json!("done")]);
    let specs = app.fake.run_specs.lock().unwrap().clone();
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].profile.video_codec, VideoCodec::Av1);
    assert_eq!(specs[1].profile.video_codec, VideoCodec::Hevc);
    assert!(
        feed(&app)
            .await
            .contains("The goal of Movies changed while a.mkv was being converted, so it was queued again for the new goal."),
    );
}

/// Converted under the old goal while the new one wants another format:
/// converted again, for the new goal.
#[tokio::test]
async fn a_conversion_for_the_old_goal_is_converted_again_for_the_new_one() {
    let app = TestApp::new().await;
    app.fake.set_behavior("a.mkv", Behavior::Hold);
    app.write("Movies/a.mkv", h264());
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib = lib["id"].as_str().unwrap().to_string();
    wait_running(&app, 1).await;
    // The next run converts it at once.
    app.fake
        .set_behavior("a.mkv", Behavior::Done { ratio: 0.5 });
    set_goal(&app, &lib, goal(Goal::Compatible)).await;
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;

    let files = app.files_by_name(&lib).await;
    let file = files["a.mp4"].clone();
    assert_eq!(file["status"], "done", "{files:?}");
    assert_eq!(file["video_codec"], "h264");
    let states: Vec<Value> = jobs_of(&app, &file)
        .await
        .iter()
        .map(|j| j["state"].clone())
        .collect();
    assert_eq!(states, [json!("done"), json!("done")]);
    let specs = app.fake.run_specs.lock().unwrap().clone();
    assert_eq!(specs[1].profile.video_codec, VideoCodec::H264);
    assert!(!specs[1].force);
    assert!(app.media.join("Movies/a.mp4").exists());
    assert!(!app.media.join("Movies/a.mkv").exists());
}

/// Converted under the old goal, and the new goal leaves the result as it
/// is: it stays done, nothing more runs.
#[tokio::test]
async fn a_conversion_the_new_goal_accepts_stays_done() {
    let app = TestApp::new().await;
    app.fake.set_behavior("a.mkv", Behavior::Hold);
    app.write("Movies/a.mkv", h264());
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib = lib["id"].as_str().unwrap().to_string();
    wait_running(&app, 1).await;
    let mut better = goal(Goal::SaveSpace);
    better["quality"] = json!("high");
    set_goal(&app, &lib, better).await;
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;

    let file = app.files_by_name(&lib).await["a.mkv"].clone();
    assert_eq!(file["status"], "done", "{file}");
    assert_eq!(jobs_of(&app, &file).await.len(), 1);
    assert!(!feed(&app).await.contains("The goal of Movies changed"));
}

/// The goal changed and changed back while the file was converting: the
/// job's verdict is the current goal's, so it stands.
#[tokio::test]
async fn a_goal_changed_back_leaves_the_verdict_as_it_is() {
    let app = TestApp::new().await;
    app.fake.set_behavior(
        "a.mkv",
        Behavior::HoldThen(Box::new(Behavior::Skip(SIZE_RULE.into()))),
    );
    app.write("Movies/a.mkv", h264());
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib_id = lib["id"].as_str().unwrap().to_string();
    wait_running(&app, 1).await;
    set_goal(&app, &lib_id, goal(Goal::Compatible)).await;
    set_goal(&app, &lib_id, lib["profile"].clone()).await;
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;

    let file = app.files_by_name(&lib_id).await["a.mkv"].clone();
    assert_eq!(file["status"], "skipped");
    assert_eq!(file["skip_reason"], SIZE_RULE);
    // Nor does a rescan try it again.
    app.rescan(&lib_id).await;
    app.wait_queue_idle().await;
    let file = app.files_by_name(&lib_id).await["a.mkv"].clone();
    assert_eq!(file["skip_reason"], SIZE_RULE);
    assert_eq!(jobs_of(&app, &file).await.len(), 1);
}

/// "Convert anyway" applies to the goal it was given: converted anyway
/// under the old goal, the file is then judged by the new goal's usual
/// rules (converted again, without "anyway"), and nothing loops.
#[tokio::test]
async fn convert_anyway_under_the_old_goal_is_judged_by_the_new_goal_once() {
    let app = TestApp::new().await;
    app.write("Movies/a.mkv", av1());
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib = lib["id"].as_str().unwrap().to_string();
    let file = app.files_by_name(&lib).await["a.mkv"].clone();
    assert_eq!(file["skip_reason"], already(VideoCodec::Av1));
    app.fake.set_behavior("a.mkv", Behavior::Hold);
    let id = file["id"].as_str().unwrap();
    let r = app
        .post(&format!("/api/files/{id}/queue"), json!({ "force": true }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    wait_running(&app, 1).await;
    app.fake
        .set_behavior("a.mkv", Behavior::Done { ratio: 0.5 });
    set_goal(&app, &lib, goal(Goal::Balanced)).await;
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;
    // Give a loop the chance to show.
    tokio::time::sleep(Duration::from_millis(300)).await;
    app.wait_queue_idle().await;

    let file = app.files_by_name(&lib).await["a.mkv"].clone();
    assert_eq!(file["status"], "done", "{file}");
    assert_eq!(file["video_codec"], "hevc");
    let jobs = jobs_of(&app, &file).await;
    assert_eq!(jobs.len(), 2, "{jobs:?}");
    assert_eq!(jobs[0]["force"], true);
    assert_eq!(jobs[1]["force"], false);
    let specs = app.fake.run_specs.lock().unwrap().clone();
    assert_eq!(specs.len(), 2);
    assert!(specs[0].force && !specs[1].force);
}

/// A failure by the goal's own rule (the chosen hardware can't make its
/// codec) is decided again for the new goal; other failures stay.
#[tokio::test]
async fn a_failure_by_the_old_goals_rule_is_decided_again() {
    let app = TestApp::new().await;
    app.fake.set_behavior(
        "a.mkv",
        Behavior::HoldThen(Box::new(Behavior::FailWith(
            ProblemKind::HardwareUnavailable,
            "You chose NVIDIA NVENC, but no working NVIDIA encoder was found here, and \
             converting on the CPU instead is turned off, so this file wasn't converted."
                .into(),
        ))),
    );
    app.fake.set_behavior(
        "b.mkv",
        Behavior::HoldThen(Box::new(Behavior::Fail("The encoder stopped.".into()))),
    );
    app.write("Movies/a.mkv", h264());
    app.write("Movies/b.mkv", h264());
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib = lib["id"].as_str().unwrap().to_string();
    wait_running(&app, 2).await;
    set_goal(&app, &lib, goal(Goal::Compatible)).await;
    app.fake.release.add_permits(2);
    app.wait_queue_idle().await;

    let files = app.files_by_name(&lib).await;
    assert_eq!(files["a.mkv"]["status"], "skipped", "{files:?}");
    assert_eq!(files["a.mkv"]["skip_reason"], already(VideoCodec::H264));
    assert_eq!(files["a.mkv"]["problem"], Value::Null);
    assert_eq!(files["b.mkv"]["status"], "failed");
    assert_eq!(files["b.mkv"]["problem"], "encoder");
}

/// A cancelled job leaves its file waiting; the next scan decides it for the
/// current goal.
#[tokio::test]
async fn a_cancelled_job_under_the_old_goal_is_decided_by_the_next_scan() {
    let app = TestApp::new().await;
    app.fake.set_behavior("a.mkv", Behavior::Hold);
    app.write("Movies/a.mkv", h264());
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib = lib["id"].as_str().unwrap().to_string();
    wait_running(&app, 1).await;
    set_goal(&app, &lib, goal(Goal::Compatible)).await;
    let job = app.get("/api/jobs?state=running").await.json["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = app.post_empty(&format!("/api/jobs/{job}/cancel")).await;
    assert_eq!(r.json["state"], "cancelled", "{}", r.text);
    app.wait_queue_idle().await;
    let file = app.files_by_name(&lib).await["a.mkv"].clone();
    assert_eq!(file["status"], "pending");

    app.rescan(&lib).await;
    let file = app.files_by_name(&lib).await["a.mkv"].clone();
    assert_eq!(file["status"], "skipped", "{file}");
    assert_eq!(file["skip_reason"], already(VideoCodec::H264));
}

/// A rescan decides again the skipped and pending files whose verdict was
/// decided with another goal than the library's (or with one that isn't
/// known), and leaves the others alone.
#[tokio::test]
async fn a_rescan_decides_verdicts_of_another_goal_again() {
    let app = TestApp::new().await;
    for name in ["c.mkv", "d.mkv", "e.mkv"] {
        app.write(&format!("Movies/{name}"), h264());
    }
    let lib = app
        .add_library("Movies", json!({ "goal": "compatible" }))
        .await;
    let lib = lib["id"].as_str().unwrap().to_string();
    let files = app.files_by_name(&lib).await;
    for name in ["c.mkv", "d.mkv", "e.mkv"] {
        assert_eq!(files[name]["skip_reason"], already(VideoCodec::H264));
    }
    // Verdicts as an earlier goal (or an unknown one) left them.
    let save_space =
        crate::db::files::profile_json(&TranscodeProfile::from_goal(Goal::SaveSpace)).unwrap();
    for (name, verdict) in [
        ("c.mkv", Some(save_space.as_str())),
        ("d.mkv", None),
        ("e.mkv", Some("current")),
    ] {
        sqlx::query(
            "UPDATE files SET skip_reason = 'Already AV1', \
             verdict_profile = CASE WHEN ?1 = 'current' THEN verdict_profile ELSE ?1 END \
             WHERE file_name = ?2",
        )
        .bind(verdict)
        .bind(name)
        .execute(app.state.db.pool())
        .await
        .unwrap();
    }
    app.rescan(&lib).await;
    let files = app.files_by_name(&lib).await;
    assert_eq!(files["c.mkv"]["skip_reason"], already(VideoCodec::H264));
    assert_eq!(files["d.mkv"]["skip_reason"], already(VideoCodec::H264));
    // Decided with the current goal: not looked at again.
    assert_eq!(files["e.mkv"]["skip_reason"], "Already AV1");
}

/// A goal change that doesn't touch what the size rule would say keeps a
/// size-rule skip, and later scans don't convert it again either; one that
/// does queues it.
#[tokio::test]
async fn a_size_rule_skip_is_only_tried_again_when_the_size_could_change() {
    let app = TestApp::new().await;
    app.fake
        .set_behavior("a.mkv", Behavior::Skip(SIZE_RULE.into()));
    app.write("Movies/a.mkv", h264());
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib_id = lib["id"].as_str().unwrap().to_string();
    app.wait_queue_idle().await;
    let file = app.files_by_name(&lib_id).await["a.mkv"].clone();
    assert_eq!(file["skip_reason"], SIZE_RULE);

    let mut subtitles = lib["profile"].clone();
    subtitles["subtitle_languages"] = json!(["eng"]);
    set_goal(&app, &lib_id, subtitles.clone()).await;
    app.rescan(&lib_id).await;
    app.wait_queue_idle().await;
    let file = app.files_by_name(&lib_id).await["a.mkv"].clone();
    assert_eq!(file["skip_reason"], SIZE_RULE);
    assert_eq!(jobs_of(&app, &file).await.len(), 1);

    subtitles["quality"] = json!("smallest");
    app.fake
        .set_behavior("a.mkv", Behavior::Done { ratio: 0.5 });
    set_goal(&app, &lib_id, subtitles).await;
    app.wait_queue_idle().await;
    let file = app.files_by_name(&lib_id).await["a.mkv"].clone();
    assert_eq!(file["status"], "done", "{file}");
    assert_eq!(jobs_of(&app, &file).await.len(), 2);
}

/// A size-rule skip whose goal isn't known (what the version 10 migration
/// leaves in a library whose goal has no size rule, such as the acceptance
/// tester's file) is decided again by the next scan: here it is converted.
#[tokio::test]
async fn a_size_rule_skip_of_an_unknown_goal_is_decided_again_by_a_rescan() {
    let app = TestApp::new().await;
    app.fake
        .set_behavior("f.mkv", Behavior::Skip(SIZE_RULE.into()));
    app.write("Movies/f.mkv", av1());
    let lib = app
        .add_library("Movies", json!({ "goal": "compatible" }))
        .await;
    let lib = lib["id"].as_str().unwrap().to_string();
    app.wait_queue_idle().await;
    let file = app.files_by_name(&lib).await["f.mkv"].clone();
    assert_eq!(file["skip_reason"], SIZE_RULE, "{file}");
    // A rescan leaves a verdict of the current goal alone.
    app.rescan(&lib).await;
    app.wait_queue_idle().await;
    let file = app.files_by_name(&lib).await["f.mkv"].clone();
    assert_eq!(file["skip_reason"], SIZE_RULE);
    assert_eq!(jobs_of(&app, &file).await.len(), 1);
    // One whose goal isn't known is decided again.
    sqlx::query("UPDATE files SET verdict_profile = NULL WHERE file_name = 'f.mkv'")
        .execute(app.state.db.pool())
        .await
        .unwrap();
    app.fake
        .set_behavior("f.mkv", Behavior::Done { ratio: 0.5 });
    app.rescan(&lib).await;
    app.wait_queue_idle().await;
    let files = app.files_by_name(&lib).await;
    let file = &files["f.mp4"];
    assert_eq!(file["status"], "done", "{files:?}");
    assert_eq!(jobs_of(&app, file).await.len(), 2);
}

/// Without auto-queue, a skipped file the new goal would convert waits for
/// the user (`pending`), and a converted one stays converted.
#[tokio::test]
async fn without_auto_queue_the_new_goal_waits_for_the_user() {
    let app = TestApp::new().await;
    let r = app
        .patch("/api/settings", json!({ "auto_queue": false }))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    app.fake.set_behavior(
        "a.mkv",
        Behavior::HoldThen(Box::new(Behavior::Skip(SIZE_RULE.into()))),
    );
    app.fake.set_behavior("b.mkv", Behavior::Hold);
    app.write("Movies/a.mkv", h264());
    app.write("Movies/b.mkv", h264());
    let lib = app
        .add_library("Movies", json!({ "goal": "save_space" }))
        .await;
    let lib = lib["id"].as_str().unwrap().to_string();
    let files = app.files_by_name(&lib).await;
    for name in ["a.mkv", "b.mkv"] {
        assert_eq!(files[name]["status"], "pending");
        let id = files[name]["id"].as_str().unwrap();
        let r = app.post_empty(&format!("/api/files/{id}/queue")).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }
    wait_running(&app, 2).await;
    set_goal(&app, &lib, goal(Goal::Balanced)).await;
    app.fake.release.add_permits(2);
    app.wait_queue_idle().await;

    let files = app.files_by_name(&lib).await;
    assert_eq!(files["a.mkv"]["status"], "pending", "{files:?}");
    assert_eq!(files["a.mkv"]["skip_reason"], Value::Null);
    assert_eq!(files["b.mkv"]["status"], "done");
    assert_eq!(files["b.mkv"]["video_codec"], "av1");
    assert_eq!(app.fake.started().len(), 2);
    assert!(feed(&app).await.contains(
        "The goal of Movies changed while a.mkv was being converted. The new goal would convert \
         it, so it is back among the files to convert."
    ));
}
