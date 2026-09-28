//! User actions on files and jobs: queue, skip, bulk changes, cancel,
//! priority, clearing history, and "Stop now".

use std::time::Duration;

use chrysopoeia_core::{ActivityLevel, Event, FileStatus, Job, JobState, MediaFile, QueueState};
use serde::Deserialize;
use uuid::Uuid;

use crate::db::activity::ActivityRefs;
use crate::db::jobs::{JobFinish, NewJob};
use crate::db::{self};
use crate::error::{ApiError, ApiResult};
use crate::format::plural;
use crate::services::dispatcher::{self, CancelIntent};
use crate::services::library::USER_SKIP_REASON;
use crate::state::AppState;

/// How long an API call waits for a running job to wind down after
/// cancelling it (ffmpeg gets 5 s before it is killed).
pub const CANCEL_WAIT: Duration = Duration::from_secs(8);

fn file_not_found() -> ApiError {
    ApiError::not_found("file_not_found", "There's no file with that id.")
}

fn job_not_found() -> ApiError {
    ApiError::not_found("job_not_found", "There's no job with that id.")
}

async fn announce_file_change(state: &AppState, file: &MediaFile) {
    state.broadcast_file(file.id).await;
    state.broadcast_library(file.library_id).await;
    state.broadcast_stats().await;
    state.broadcast_queue_state().await;
}

/// Queue a file (pending, failed, skipped or done). 409 when it is already
/// queued or processing.
pub async fn queue_file(state: &AppState, file_id: Uuid, priority: Option<i32>) -> ApiResult<Job> {
    let file = db::files::get(state.db.pool(), file_id, false)
        .await?
        .ok_or_else(file_not_found)?;
    if matches!(file.status, FileStatus::Queued | FileStatus::Processing) {
        return Err(ApiError::conflict(
            "already_queued",
            "This file is already in the queue.",
        ));
    }
    let mut tx = state.db.write_tx().await?;
    let created = db::jobs::create(
        &mut tx,
        &NewJob {
            file_id: file.id,
            library_id: file.library_id,
            file_name: &file.file_name,
            file_path: &file.path,
            input_size: file.size_bytes,
            priority: priority.unwrap_or(0),
        },
    )
    .await?;
    tx.commit().await?;
    let Some(job_id) = created else {
        return Err(ApiError::conflict(
            "already_queued",
            "This file is already in the queue.",
        ));
    };
    state.dispatcher.wake();
    let job = db::jobs::get(state.db.pool(), job_id)
        .await?
        .ok_or_else(job_not_found)?;
    state.emit(Event::JobUpdated { job: job.clone() });
    announce_file_change(state, &file).await;
    Ok(job)
}

/// Mark a file "Skipped by you", cancelling its queued or running job.
pub async fn skip_file(state: &AppState, file_id: Uuid) -> ApiResult<MediaFile> {
    let file = db::files::get(state.db.pool(), file_id, false)
        .await?
        .ok_or_else(file_not_found)?;
    match file.status {
        FileStatus::Processing => {
            let ids = state.dispatcher.cancel_file(file.id, CancelIntent::Skip);
            if ids.is_empty() {
                // Not actually running (stale row): fix it directly.
                let mut tx = state.db.write_tx().await?;
                db::files::set_status(
                    &mut tx,
                    file.id,
                    FileStatus::Skipped,
                    Some(USER_SKIP_REASON),
                    None,
                )
                .await?;
                tx.commit().await?;
            } else {
                state.dispatcher.wait_finished(&ids, CANCEL_WAIT).await;
            }
        }
        FileStatus::Queued => {
            let mut tx = state.db.write_tx().await?;
            let cancelled = match file.job_id {
                Some(job_id) => {
                    db::jobs::cancel_queued(
                        &mut tx,
                        job_id,
                        FileStatus::Skipped,
                        Some(USER_SKIP_REASON),
                    )
                    .await?
                }
                None => false,
            };
            if !cancelled {
                db::files::set_status(
                    &mut tx,
                    file.id,
                    FileStatus::Skipped,
                    Some(USER_SKIP_REASON),
                    None,
                )
                .await?;
            }
            tx.commit().await?;
            if let Some(job_id) = file.job_id {
                state.broadcast_job(job_id).await;
            }
        }
        _ => {
            let mut tx = state.db.write_tx().await?;
            db::files::set_status(
                &mut tx,
                file.id,
                FileStatus::Skipped,
                Some(USER_SKIP_REASON),
                None,
            )
            .await?;
            tx.commit().await?;
        }
    }
    announce_file_change(state, &file).await;
    db::files::get(state.db.pool(), file_id, false)
        .await?
        .ok_or_else(file_not_found)
}

/// A bulk action on files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BulkAction {
    Queue,
    Skip,
    RetryFailed,
}

/// Selection for a bulk action.
#[derive(Debug, Clone, Default)]
pub struct BulkSelection {
    pub ids: Option<Vec<Uuid>>,
    pub library: Option<Uuid>,
    pub statuses: Vec<FileStatus>,
}

fn intersect(requested: &[FileStatus], allowed: &[FileStatus]) -> Vec<FileStatus> {
    if requested.is_empty() {
        allowed.to_vec()
    } else {
        requested
            .iter()
            .copied()
            .filter(|s| allowed.contains(s))
            .collect()
    }
}

/// Apply a bulk action. Without ids or a status filter, `queue` only takes
/// pending files and `skip` pending and queued files, so a bare request can
/// never re-convert finished files. Returns how many files changed.
pub async fn bulk(state: &AppState, action: BulkAction, sel: BulkSelection) -> ApiResult<u64> {
    use FileStatus::{Done, Failed, Pending, Processing, Queued, Skipped};
    let explicit = sel.ids.is_some();
    let statuses = match action {
        BulkAction::Queue if explicit || !sel.statuses.is_empty() => {
            intersect(&sel.statuses, &[Pending, Failed, Skipped, Done])
        }
        BulkAction::Queue => vec![Pending],
        BulkAction::Skip if explicit => {
            intersect(&sel.statuses, &[Pending, Queued, Failed, Processing])
        }
        BulkAction::Skip if !sel.statuses.is_empty() => {
            intersect(&sel.statuses, &[Pending, Queued, Failed])
        }
        BulkAction::Skip => vec![Pending, Queued],
        BulkAction::RetryFailed => vec![Failed],
    };
    if statuses.is_empty() {
        return Ok(0);
    }
    let ids =
        db::files::select_ids(state.db.pool(), sel.ids.as_deref(), sel.library, &statuses).await?;
    if ids.is_empty() {
        return Ok(0);
    }
    let files = db::files::get_many(state.db.pool(), &ids).await?;
    let mut affected = 0u64;
    let mut running_to_cancel = Vec::new();
    let mut tx = state.db.write_tx().await?;
    for file in &files {
        match action {
            BulkAction::Queue | BulkAction::RetryFailed => {
                let created = db::jobs::create(
                    &mut tx,
                    &NewJob {
                        file_id: file.id,
                        library_id: file.library_id,
                        file_name: &file.file_name,
                        file_path: &file.path,
                        input_size: file.size_bytes,
                        priority: 0,
                    },
                )
                .await?;
                affected += u64::from(created.is_some());
            }
            BulkAction::Skip => match file.status {
                Processing => running_to_cancel.push(file.id),
                Queued => {
                    let cancelled = match file.job_id {
                        Some(job_id) => {
                            db::jobs::cancel_queued(
                                &mut tx,
                                job_id,
                                Skipped,
                                Some(USER_SKIP_REASON),
                            )
                            .await?
                        }
                        None => false,
                    };
                    if !cancelled {
                        db::files::set_status(
                            &mut tx,
                            file.id,
                            Skipped,
                            Some(USER_SKIP_REASON),
                            None,
                        )
                        .await?;
                    }
                    affected += 1;
                }
                _ => {
                    db::files::set_status(&mut tx, file.id, Skipped, Some(USER_SKIP_REASON), None)
                        .await?;
                    affected += 1;
                }
            },
        }
    }
    tx.commit().await?;
    let mut cancelled_jobs = Vec::new();
    for file_id in running_to_cancel {
        let ids = state.dispatcher.cancel_file(file_id, CancelIntent::Skip);
        affected += u64::from(!ids.is_empty());
        cancelled_jobs.extend(ids);
    }
    if !cancelled_jobs.is_empty() {
        state
            .dispatcher
            .wait_finished(&cancelled_jobs, CANCEL_WAIT)
            .await;
    }
    if affected > 0 {
        state.dispatcher.wake();
        state.emit(Event::FilesChanged {
            library_id: sel.library,
        });
        match sel.library {
            Some(lib) => state.broadcast_library(lib).await,
            None => {
                for lib in crate::services::library::view_all(state).await? {
                    state.emit(Event::LibraryUpdated { library: lib });
                }
            }
        }
        state.broadcast_stats().await;
        state.broadcast_queue_state().await;
    }
    Ok(affected)
}

/// Cancel a queued or running job.
pub async fn cancel_job(state: &AppState, id: Uuid) -> ApiResult<Job> {
    let job = db::jobs::get(state.db.pool(), id)
        .await?
        .ok_or_else(job_not_found)?;
    let refs = ActivityRefs {
        file_id: Some(job.file_id),
        job_id: Some(job.id),
        library_id: Some(job.library_id),
    };
    match job.state {
        JobState::Queued => {
            let mut tx = state.db.write_tx().await?;
            let cancelled = db::jobs::cancel_queued(&mut tx, id, FileStatus::Pending, None).await?;
            tx.commit().await?;
            if cancelled {
                state
                    .activity(
                        ActivityLevel::Info,
                        format!("Removed {} from the queue", job.file_name),
                        refs,
                    )
                    .await;
            }
        }
        JobState::Running => {
            if state.dispatcher.cancel_job(id, CancelIntent::User) {
                state.dispatcher.wait_finished(&[id], CANCEL_WAIT).await;
            } else {
                // A row marked running without a task (should not happen
                // after start-up recovery): close it directly.
                let mut tx = state.db.write_tx().await?;
                db::jobs::finish(&mut tx, id, JobState::Cancelled, &JobFinish::default()).await?;
                db::files::set_status(&mut tx, job.file_id, FileStatus::Pending, None, None)
                    .await?;
                tx.commit().await?;
            }
        }
        _ => {
            return Err(ApiError::conflict(
                "job_finished",
                "This job has already finished.",
            ));
        }
    }
    let job = db::jobs::get(state.db.pool(), id)
        .await?
        .ok_or_else(job_not_found)?;
    state.emit(Event::JobUpdated { job: job.clone() });
    state.broadcast_file(job.file_id).await;
    state.broadcast_library(job.library_id).await;
    state.broadcast_stats().await;
    state.broadcast_queue_state().await;
    Ok(job)
}

/// A priority change: an explicit value or "move to top".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorityChange {
    Set(i32),
    Top,
}

/// Change a queued (or running) job's priority.
pub async fn set_priority(state: &AppState, id: Uuid, change: PriorityChange) -> ApiResult<Job> {
    let job = db::jobs::get(state.db.pool(), id)
        .await?
        .ok_or_else(job_not_found)?;
    if job.state.is_finished() {
        return Err(ApiError::conflict(
            "job_finished",
            "This job has already finished.",
        ));
    }
    let priority = match change {
        PriorityChange::Set(p) => p,
        PriorityChange::Top => db::jobs::max_queued_priority(state.db.pool())
            .await?
            .unwrap_or(0)
            .saturating_add(1),
    };
    db::jobs::set_priority(state.db.pool(), id, priority).await?;
    let job = db::jobs::get(state.db.pool(), id)
        .await?
        .ok_or_else(job_not_found)?;
    state.emit(Event::JobUpdated { job: job.clone() });
    state.broadcast_queue_state().await;
    Ok(job)
}

/// Delete finished jobs.
pub async fn clear_history(state: &AppState) -> ApiResult<u64> {
    let n = db::jobs::clear_history(&state.db).await?;
    state.broadcast_queue_state().await;
    Ok(n)
}

/// "Stop now": pause, cancel running jobs and put them back in the queue.
pub async fn stop_now(state: &AppState) -> ApiResult<QueueState> {
    dispatcher::set_paused(state, true).await?;
    let ids = state.dispatcher.cancel_all(CancelIntent::Requeue);
    if !ids.is_empty() {
        state.dispatcher.wait_finished(&ids, CANCEL_WAIT).await;
        state
            .activity(
                ActivityLevel::Info,
                format!(
                    "Stopped {}. They'll start again when you resume the queue.",
                    plural(ids.len() as u64, "conversion", "conversions")
                ),
                ActivityRefs::default(),
            )
            .await;
    }
    state.broadcast_stats().await;
    Ok(dispatcher::queue_state(state).await?)
}
