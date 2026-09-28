//! Adding, changing and removing libraries.

use std::path::{Path, PathBuf};

use chrono::Utc;
use chrysopoeia_core::{ActivityLevel, Event, Goal, Library, OutputMode, TranscodeProfile};
use uuid::Uuid;

use crate::db;
use crate::db::libraries::LibraryRow;
use crate::error::{ApiError, ApiResult};
use crate::services::dispatcher::CancelIntent;
use crate::services::library::{self, view_by_id};
use crate::services::queue::CANCEL_WAIT;
use crate::services::watcher;
use crate::state::AppState;

/// Longest accepted library name.
pub const MAX_NAME_LEN: usize = 100;

/// A library to create.
#[derive(Debug, Clone, Default)]
pub struct NewLibrary {
    pub path: String,
    pub name: Option<String>,
    pub profile: Option<TranscodeProfile>,
    pub goal: Option<Goal>,
}

/// Changes to a library.
#[derive(Debug, Clone, Default)]
pub struct LibraryPatch {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub profile: Option<TranscodeProfile>,
}

fn not_readable(path: &str) -> ApiError {
    ApiError::bad_request(
        "not_readable",
        format!(
            "Chrysopoeia can't read {path}. Check the folder's permissions (in Docker, the \
             PUID/PGID user needs access)."
        ),
    )
}

/// Check a folder for a new library and return its canonical path.
pub async fn validate_library_path(state: &AppState, raw: &str) -> ApiResult<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(ApiError::bad_request(
            "path_required",
            "Choose a folder for the library.",
        ));
    }
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(ApiError::bad_request(
            "path_not_absolute",
            "Use the folder's full path, starting with /.",
        ));
    }
    let meta = match tokio::fs::metadata(path).await {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ApiError::bad_request(
                "path_not_found",
                format!(
                    "The folder {raw} doesn't exist on the server. In Docker, check that it is \
                     mounted into the container."
                ),
            ));
        }
        Err(_) => return Err(not_readable(raw)),
    };
    if !meta.is_dir() {
        return Err(ApiError::bad_request(
            "not_a_directory",
            format!("{raw} is a file, not a folder."),
        ));
    }
    if tokio::fs::read_dir(path).await.is_err() {
        return Err(not_readable(raw));
    }
    let canonical = tokio::fs::canonicalize(path)
        .await
        .map_err(|_| not_readable(raw))?;
    if canonical.to_str().is_none() {
        return Err(ApiError::bad_request(
            "path_not_supported",
            "Folder names must be valid UTF-8 text.",
        ));
    }
    let settings = state.settings();
    if settings.output_mode == OutputMode::Folder
        && let Some(out) = settings.output_folder.as_deref()
    {
        let out = tokio::fs::canonicalize(out)
            .await
            .unwrap_or_else(|_| PathBuf::from(out));
        if out.starts_with(&canonical) {
            return Err(ApiError::bad_request(
                "contains_output_folder",
                "The output folder is inside this folder, so Chrysopoeia would convert its own \
                 results. Pick another folder, or change the output folder in Settings.",
            ));
        }
    }
    for lib in db::libraries::list(state.db.pool()).await? {
        let existing = Path::new(&lib.path);
        if canonical == existing {
            return Err(ApiError::conflict(
                "library_exists",
                format!("That folder is already the library {}.", lib.name),
            ));
        }
        if canonical.starts_with(existing) {
            return Err(ApiError::conflict(
                "library_overlaps",
                format!(
                    "That folder is inside {}, which is already a library.",
                    lib.name
                ),
            ));
        }
        if existing.starts_with(&canonical) {
            return Err(ApiError::conflict(
                "library_overlaps",
                format!(
                    "That folder contains the library {}. Pick a different folder, or remove {} first.",
                    lib.name, lib.name
                ),
            ));
        }
    }
    Ok(canonical)
}

fn clean_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request(
            "invalid_name",
            "Give the library a name.",
        ));
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(ApiError::bad_request(
            "invalid_name",
            format!("Library names can be at most {MAX_NAME_LEN} characters."),
        ));
    }
    Ok(name.to_string())
}

async fn note_adjustments(state: &AppState, name: &str, notes: &[String], id: Uuid) {
    if notes.is_empty() {
        return;
    }
    state
        .library_activity(
            ActivityLevel::Info,
            format!("Adjusted {name}'s settings: {}.", notes.join("; ")),
            id,
        )
        .await;
}

/// Create a library and start scanning it.
pub async fn create(state: &AppState, new: NewLibrary) -> ApiResult<Library> {
    let path = validate_library_path(state, &new.path).await?;
    let path_str = path.to_str().unwrap_or_default().to_string();
    let name = match new.name.as_deref().filter(|n| !n.trim().is_empty()) {
        Some(n) => clean_name(n)?,
        None => path
            .file_name()
            .map_or_else(|| path_str.clone(), |n| n.to_string_lossy().into_owned()),
    };
    let mut profile = match (new.profile, new.goal) {
        (Some(p), _) => p,
        (None, Some(goal)) => TranscodeProfile::from_goal(goal),
        (None, None) => state.settings().default_profile,
    };
    let notes = profile.normalize();
    let row = LibraryRow {
        id: Uuid::new_v4(),
        name,
        path: path_str.clone(),
        enabled: true,
        profile,
        last_scan_at: None,
        created_at: Utc::now(),
    };
    if let Err(e) = db::libraries::insert(state.db.pool(), &row).await {
        if matches!(&e, sqlx::Error::Database(d) if d.is_unique_violation()) {
            return Err(ApiError::conflict(
                "library_exists",
                "That folder is already a library.",
            ));
        }
        return Err(e.into());
    }
    state
        .library_activity(
            ActivityLevel::Info,
            format!("Added the library {} ({path_str})", row.name),
            row.id,
        )
        .await;
    note_adjustments(state, &row.name, &notes, row.id).await;
    watcher::sync(state).await;
    // A fresh library can't be scanning yet, so this always starts.
    let _ = library::start_scan(state, row.id);
    let lib = view_by_id(state, row.id)
        .await?
        .ok_or_else(|| ApiError::internal("library disappeared right after it was created"))?;
    state.emit(Event::LibraryUpdated {
        library: lib.clone(),
    });
    Ok(lib)
}

fn library_not_found() -> ApiError {
    ApiError::not_found("library_not_found", "There's no library with that id.")
}

/// Rename, enable/disable or change the profile of a library. A profile
/// change re-decides its pending and skipped files.
pub async fn update(state: &AppState, id: Uuid, patch: LibraryPatch) -> ApiResult<Library> {
    let mut row = db::libraries::get(state.db.pool(), id)
        .await?
        .ok_or_else(library_not_found)?;
    let was_enabled = row.enabled;
    if let Some(name) = &patch.name {
        row.name = clean_name(name)?;
    }
    if let Some(enabled) = patch.enabled {
        row.enabled = enabled;
    }
    let mut notes = Vec::new();
    let previous_profile = row.profile.clone();
    if let Some(mut profile) = patch.profile {
        notes = profile.normalize();
        row.profile = profile;
    }
    let profile_changed = row.profile != previous_profile;
    db::libraries::update(state.db.pool(), &row).await?;
    note_adjustments(state, &row.name, &notes, id).await;

    if profile_changed {
        let changed = library::redecide(state, &row, Some(&previous_profile)).await?;
        if changed > 0 {
            state.emit(Event::FilesChanged {
                library_id: Some(id),
            });
            state.broadcast_stats().await;
        }
    }
    if was_enabled != row.enabled {
        watcher::sync(state).await;
        if row.enabled {
            state.dispatcher.wake();
            if row.last_scan_at.is_none() {
                let _ = library::start_scan(state, id);
            }
        } else {
            state.library.cancel_scan(id);
        }
    }
    let lib = view_by_id(state, id).await?.ok_or_else(library_not_found)?;
    state.emit(Event::LibraryUpdated {
        library: lib.clone(),
    });
    state.broadcast_queue_state().await;
    Ok(lib)
}

/// Remove a library from Chrysopoeia. Media files are never touched.
pub async fn delete(state: &AppState, id: Uuid) -> ApiResult<()> {
    let row = db::libraries::get(state.db.pool(), id)
        .await?
        .ok_or_else(library_not_found)?;
    state.library.cancel_scan(id);
    let running = state.dispatcher.cancel_library(id, CancelIntent::Removed);
    db::libraries::delete(state.db.pool(), id).await?;
    if !running.is_empty() {
        state.dispatcher.wait_finished(&running, CANCEL_WAIT).await;
    }
    watcher::sync(state).await;
    state
        .library_activity(
            ActivityLevel::Info,
            format!(
                "Removed the library {}. Its files on disk were not touched.",
                row.name
            ),
            id,
        )
        .await;
    state.emit(Event::LibraryRemoved { id });
    state.emit(Event::FilesChanged { library_id: None });
    state.broadcast_stats().await;
    state.broadcast_queue_state().await;
    Ok(())
}
