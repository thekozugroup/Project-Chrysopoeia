//! A job's attempt history and how its progress was worked out: each way
//! of converting a file that a job tried is stored with how it ended, in
//! the job API and the events, while the next attempt still runs (the
//! Atlas report: a GPU's file that failed a check looked like a hardware
//! problem, because only the last attempt was kept).

use chrysopoeia_core::{AttemptResult, Event, JobState, ProgressBasis};
use serde_json::{Value, json};

use super::support::*;

/// The running jobs.
async fn running(app: &TestApp) -> Vec<Value> {
    app.get("/api/jobs?state=running").await.json["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn each_attempt_is_kept_and_shown_while_the_next_one_runs() {
    let app = TestApp::new().await;
    let mut events = app.state.events.subscribe();
    app.fake.set_behavior("Kingsman.mkv", Behavior::FallBack);
    app.write("Movies/Kingsman.mkv", h264());
    app.add_library("Movies", json!({})).await;

    // The GPU's attempt failed a check; the CPU's runs.
    wait_until("the first attempt to be stored", || async {
        running(&app)
            .await
            .first()
            .is_some_and(|j| j["attempts"].as_array().is_some_and(|a| a.len() == 1))
    })
    .await;
    let job = running(&app).await.remove(0);
    let id = job["id"].as_str().unwrap().to_string();
    assert_eq!(job["attempt"], 2, "{job}");
    let first = &job["attempts"][0];
    assert_eq!(first["attempt"], 1);
    assert_eq!(first["encoder"], "hevc_vaapi");
    assert_eq!(first["hw_api"], "vaapi");
    assert_eq!(first["device"], "/dev/dri/renderD128");
    assert_eq!(first["hw_decode"], true);
    assert_eq!(first["elapsed_secs"], 128.4);
    assert_eq!(first["result"], "failed");
    assert_eq!(first["problem"], "verification");
    assert_eq!(first["failed_check"]["id"], "decode");
    assert_eq!(first["failed_check"]["label"], "Plays start to finish");
    assert_eq!(
        first["failed_check"]["detail"],
        "Playback stopped at 1:01 of 2:21:02"
    );
    assert!(
        first["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("The new file doesn't play start to finish")),
        "{first}"
    );
    assert!(
        first["command"]
            .as_str()
            .is_some_and(|c| c.contains("-hwaccel vaapi"))
    );
    assert_eq!(first["log_tail"], "[warning] Invalid timestamps");
    // The CPU can't say how far it is yet: the job says so, with the frames
    // and the time spent, instead of a plain 0 %.
    assert_eq!(job["progress_basis"], "unknown", "{job}");
    assert_eq!(job["frames"], 366);
    assert_eq!(job["elapsed_secs"], 99);
    // The job read by itself says the same.
    let one = app.get(&format!("/api/jobs/{id}")).await.json;
    assert_eq!(one["attempts"], job["attempts"]);
    assert_eq!(one["progress_basis"], "unknown");

    // Done: both attempts, the one that made the file last; the live
    // fields go with the run.
    app.fake.release.add_permits(1);
    app.wait_queue_idle().await;
    let done = app.get(&format!("/api/jobs/{id}")).await.json;
    assert_eq!(done["state"], "done", "{done}");
    let attempts = done["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 2, "{done}");
    assert_eq!(attempts[0], job["attempts"][0]);
    assert_eq!(attempts[1]["result"], "succeeded");
    assert!(attempts[1]["error"].is_null());
    assert!(done["progress_basis"].is_null(), "{done}");
    assert!(
        done["frames"].is_null() && done["elapsed_secs"].is_null(),
        "{done}"
    );
    // The job's own final fields are as before: the attempt that made it.
    assert_eq!(done["encoder"], attempts[1]["encoder"]);
    assert_eq!(done["command"], attempts[1]["command"]);
    assert!(
        done["command"]
            .as_str()
            .is_some_and(|c| c.starts_with("ffmpeg -i "))
    );
    // The file's page and the history list carry them too.
    let file_id = done["file_id"].as_str().unwrap();
    let detail = app.get(&format!("/api/files/{file_id}")).await.json;
    assert_eq!(detail["jobs"][0]["attempts"], done["attempts"]);
    let history = app.get("/api/jobs?state=history").await.json;
    assert_eq!(history["items"][0]["attempts"], done["attempts"]);

    // The events: the job, with its first attempt, went out while the CPU
    // ran, and again with both when it finished; progress events carry the
    // basis and frames but never the attempts.
    let (mut running_with_one, mut finished_with_two, mut progress_seen) = (false, false, false);
    while let Ok(event) = events.try_recv() {
        let json = serde_json::to_value(&event).unwrap();
        match event {
            Event::JobUpdated { job } if job.state == JobState::Running => {
                running_with_one |= job.attempts.len() == 1
                    && job.attempts[0].result == AttemptResult::Failed
                    && job.progress_basis == Some(ProgressBasis::Unknown);
            }
            Event::JobUpdated { job } if job.state == JobState::Done => {
                finished_with_two |= job.attempts.len() == 2;
            }
            Event::JobProgress(p) => {
                assert!(json.get("attempts").is_none(), "{json}");
                if p.attempt == 2 {
                    progress_seen = true;
                    assert_eq!(json["progress_basis"], "unknown", "{json}");
                    assert_eq!(json["frames"], 366, "{json}");
                    assert_eq!(json["elapsed_secs"], 99, "{json}");
                }
            }
            _ => {}
        }
    }
    assert!(
        running_with_one,
        "a job.updated with the first attempt while it ran"
    );
    assert!(
        finished_with_two,
        "a job.updated with both attempts when it finished"
    );
    assert!(progress_seen, "the second attempt's progress went out");
}

/// A job that goes back to the queue starts its next run afresh: the
/// attempts of a run that was stopped belong to no result.
#[tokio::test]
async fn a_job_put_back_in_the_queue_starts_a_fresh_history() {
    let app = TestApp::new().await;
    app.fake.set_behavior("Kingsman.mkv", Behavior::FallBack);
    app.write("Movies/Kingsman.mkv", h264());
    app.add_library("Movies", json!({})).await;
    wait_until("the first attempt to be stored", || async {
        running(&app)
            .await
            .first()
            .is_some_and(|j| j["attempts"].as_array().is_some_and(|a| a.len() == 1))
    })
    .await;
    let id = running(&app).await[0]["id"].as_str().unwrap().to_string();
    let r = app.post_empty("/api/queue/stop").await;
    assert_eq!(r.status, axum::http::StatusCode::OK, "{}", r.text);
    wait_until("the job to be back in the queue", || async {
        app.get(&format!("/api/jobs/{id}")).await.json["state"] == "queued"
    })
    .await;
    let job = app.get(&format!("/api/jobs/{id}")).await.json;
    assert_eq!(job["attempts"], json!([]), "{job}");
    assert!(
        job["progress_basis"].is_null() && job["frames"].is_null(),
        "{job}"
    );
}
