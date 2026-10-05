//! Settings changes: merge, validate, persist, and apply.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use szalinski_core::{ActivityLevel, Event, FolderSetting, FolderStatus, OutputMode, Settings};
use szalinski_scanner::validate_ignore_pattern;

use crate::db;
use crate::db::activity::ActivityRefs;
use crate::error::{ApiError, ApiResult, describe_value_error};
use crate::services::dispatcher::MAX_JOBS_LIMIT;
use szalinski_worker::slow_fs;

use crate::services::fs_guard::{self, NoAnswer};
use crate::services::share_mounts::{self, Mounted};
use crate::services::{hardware, watcher};
use crate::state::AppState;

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::bad_request("invalid_settings", message)
}

/// An invalid value for the setting `field`.
fn invalid_field(field: &'static str, message: impl Into<String>) -> ApiError {
    invalid(message).with_field(field)
}

/// Merge a partial settings object onto `current` (top level only; nested
/// objects such as `default_profile` are replaced whole).
pub fn merge(current: &Settings, patch: Value) -> ApiResult<Settings> {
    let Value::Object(patch) = patch else {
        return Err(invalid("Send the settings to change as a JSON object."));
    };
    let Ok(Value::Object(mut merged)) = serde_json::to_value(current) else {
        return Err(ApiError::internal(
            "settings did not serialize to an object",
        ));
    };
    for key in patch.keys() {
        if !merged.contains_key(key) {
            return Err(ApiError::bad_request(
                "unknown_setting",
                format!("There's no setting called \"{key}\"."),
            )
            .with_field(key.clone()));
        }
    }
    for (k, v) in patch {
        merged.insert(k, v);
    }
    // The path of the value at fault, down to nested fields such as
    // `default_profile.quality`, so the UI can point at the control.
    serde_path_to_error::deserialize::<_, Settings>(Value::Object(merged)).map_err(|e| {
        let path = e.path().to_string();
        let serde_message = e.inner().to_string();
        let field = (!path.is_empty() && path != ".").then_some(path);
        let error = invalid(describe_value_error(field.as_deref(), &serde_message));
        match field {
            Some(f) => error.with_field(f),
            None => error,
        }
    })
}

fn blank_to_none(value: &mut Option<String>) {
    if value.as_deref().is_some_and(|v| v.trim().is_empty()) {
        *value = None;
    } else if let Some(v) = value {
        *v = v.trim().to_string();
    }
}

/// How long a folder chosen in Settings may take to answer the checks
/// below: a share that stopped answering doesn't hold the save.
const FOLDER_CHECK_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(2)
} else {
    Duration::from_secs(15)
};

/// What [`check_writable_dir`] found, on its blocking thread.
#[derive(Debug, Clone, Copy)]
enum Writable {
    Yes,
    NotAFolder,
    Missing,
    ReadOnly,
    Full,
    Denied,
}

fn writable_now(path: &Path) -> Writable {
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Writable::NotAFolder,
        Err(_) => return Writable::Missing,
    }
    let probe = path.join(format!(
        ".szalinski-write-check-{}",
        uuid::Uuid::new_v4().simple()
    ));
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Writable::Yes
        }
        Err(e) if e.kind() == std::io::ErrorKind::ReadOnlyFilesystem => Writable::ReadOnly,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded
            ) =>
        {
            Writable::Full
        }
        Err(_) => Writable::Denied,
    }
}

/// Check that `dir` is an existing, writable folder. `field` names the
/// setting in errors.
async fn check_writable_dir(dir: &str, what: &str, field: &'static str) -> ApiResult<()> {
    let path = Path::new(dir);
    if !path.is_absolute() {
        return Err(invalid_field(
            field,
            format!("The {what} must be a full path starting with /."),
        ));
    }
    let p = path.to_path_buf();
    let found = fs_guard::guarded("writable", path, FOLDER_CHECK_TIMEOUT, move || {
        writable_now(&p)
    })
    .await;
    let message = match found {
        Ok(Writable::Yes) => return Ok(()),
        Ok(Writable::NotAFolder) => format!("The {what} {dir} is a file, not a folder."),
        Ok(Writable::Missing) => format!(
            "The {what} {dir} doesn't exist on the server. In Docker, check that it is mounted."
        ),
        Ok(Writable::ReadOnly) => format!(
            "The {what} {dir} is on a read-only drive. Choose a folder Szalinski can write to."
        ),
        Ok(Writable::Full) => format!(
            "The {what} {dir} is on a full disk. Free up some space there, or choose another folder."
        ),
        Ok(Writable::Denied) => format!(
            "Szalinski can't write to the {what} {dir}. Check its permissions (in Docker, the \
             PUID/PGID user needs write access)."
        ),
        Err(NoAnswer::NotAnswering) => format!(
            "The {what} {dir} isn't responding. If it's on a network share or an external drive, \
             check the connection, then save again."
        ),
        Err(NoAnswer::Busy) => format!(
            "Szalinski couldn't check the {what} {dir} right now because other folders aren't \
             responding. Try again in a moment."
        ),
    };
    Err(invalid_field(field, message))
}

/// The scanner's reason an ignore pattern can't be used, as a settings
/// error: it is worded for the scan log ("…, so it was not used"), and any
/// glob library detail ("error parsing glob '…': …") is dropped so only a
/// plain reason naming the pattern is left.
fn ignore_pattern_error(reason: &str) -> String {
    let mut reason = reason.replace(", so it was not used", "");
    if let Some(start) = reason.find("error parsing glob '")
        && let Some(end) = reason[start..].find("': ")
    {
        reason.replace_range(start..start + end + 3, "");
    }
    format!("{}.", reason.trim().trim_end_matches('.'))
}

/// Which folder settings name the same folder as before (see
/// [`share_mounts::same_folder`]): saved again as they were, they keep the
/// drives and shares remembered for them.
#[derive(Debug, Clone, Copy, Default)]
pub struct Kept {
    pub output_folder: bool,
    pub temp_dir: bool,
}

/// The work folder `settings` have in use: their own, else the one the
/// server was started with.
fn work_folder(state: &AppState, settings: &Settings) -> Option<String> {
    settings.temp_dir.clone().or_else(|| {
        state
            .config
            .temp_dir
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
    })
}

/// Normalize and validate new settings. `old` are the settings they
/// replace: an ignore pattern saved before patterns were checked this
/// strictly doesn't block other changes (scans skip it and say so), and a
/// folder saved again as it was while its drive isn't connected as it was
/// (its share unmounted, another drive in its place) isn't written into to
/// check it, nor refused for that (it was checked when it was chosen; its
/// jobs wait for it meanwhile).
pub async fn validate(state: &AppState, old: &Settings, s: &mut Settings) -> ApiResult<Kept> {
    blank_to_none(&mut s.temp_dir);
    blank_to_none(&mut s.output_folder);
    // The work folder in use is the setting, else the one the server was
    // started with (TEMP_DIR): choosing that same folder in Settings is no
    // change.
    let (work_before, work_after) = (work_folder(state, old), work_folder(state, s));
    let (output_folder, temp_dir) = tokio::join!(
        share_mounts::same_folder(
            state,
            old.output_folder.as_deref(),
            s.output_folder.as_deref()
        ),
        share_mounts::same_folder(state, work_before.as_deref(), work_after.as_deref()),
    );
    let kept = Kept {
        output_folder,
        temp_dir,
    };
    s.ignore_patterns = s
        .ignore_patterns
        .iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    crate::services::library_admin::check_profile(&s.default_profile, "default_profile")?;
    s.default_profile.normalize();

    if let Some(n) = s.max_jobs
        && !(1..=MAX_JOBS_LIMIT).contains(&n)
    {
        return Err(invalid_field(
            "max_jobs",
            format!("Files at once must be between 1 and {MAX_JOBS_LIMIT}."),
        ));
    }
    if let Some(h) = s.active_hours
        && (h.start > 23 || h.end > 23)
    {
        return Err(invalid_field(
            "active_hours",
            "Active hours must be whole hours from 0 to 23.",
        ));
    }
    // The scanner's own check: it also refuses patterns that would silently
    // match nothing (backslashes, `!` exceptions).
    for p in s
        .ignore_patterns
        .iter()
        .filter(|p| !old.ignore_patterns.contains(p))
    {
        if let Err(e) = validate_ignore_pattern(p) {
            return Err(invalid_field("ignore_patterns", ignore_pattern_error(&e)));
        }
    }
    // What is remembered for a folder saved again is stored under the path
    // it was saved as.
    let away = async |kept: bool, before: Option<&str>| match before {
        Some(before) if kept => share_mounts::away(state, Path::new(before)).await,
        _ => false,
    };
    if let Some(dir) = s.temp_dir.clone()
        && !away(kept.temp_dir, work_before.as_deref()).await
    {
        check_writable_dir(&dir, "work folder", "temp_dir").await?;
    }
    if s.output_mode == OutputMode::Folder {
        let Some(dir) = s.output_folder.clone() else {
            return Err(invalid_field(
                "output_folder",
                "Choose an output folder, or switch back to replacing the originals.",
            ));
        };
        if !away(kept.output_folder, old.output_folder.as_deref()).await {
            check_writable_dir(&dir, "output folder", "output_folder").await?;
        }
        let out = slow_fs::real_path(Path::new(&dir), FOLDER_CHECK_TIMEOUT)
            .await
            .unwrap_or_else(|| Path::new(&dir).to_path_buf());
        for lib in db::libraries::list(state.db.pool()).await? {
            if out.starts_with(&lib.path) {
                return Err(invalid_field(
                    "output_folder",
                    format!(
                        "The output folder can't be inside the library {}, or Szalinski would \
                         convert its own results.",
                        lib.name
                    ),
                ));
            }
        }
    }
    Ok(kept)
}

/// Apply a partial update: merge, validate, persist, publish and apply.
pub async fn patch(state: &AppState, patch: Value) -> ApiResult<Settings> {
    let _guard = state.settings_write.lock().await;
    let old = state.settings();
    let mut new = merge(&old, patch)?;
    let kept = validate(state, &old, &mut new).await?;
    let (work_before, work_after) = (work_folder(state, &old), work_folder(state, &new));
    let folders = [
        (
            FolderSetting::OutputFolder,
            &old.output_folder,
            &new.output_folder,
            kept.output_folder,
        ),
        (
            FolderSetting::TempDir,
            &work_before,
            &work_after,
            kept.temp_dir,
        ),
    ];
    // The same folder given another way (through a link, say) keeps what
    // is remembered for it, from before anything looks at it that way.
    for (_, before, after, same) in folders {
        if let (true, Some(before), Some(after)) = (same, before, after)
            && before != after
        {
            share_mounts::carry_over(state, Path::new(before), Path::new(after)).await;
        }
    }
    db::settings::save(state.db.pool(), &new).await?;
    state.replace_settings(new.clone());
    state.emit(Event::SettingsUpdated {
        settings: Box::new(new.clone()),
    });

    if old.hardware != new.hardware || old.cpu_fallback != new.cpu_fallback {
        hardware::apply_preference(state, new.hardware, new.cpu_fallback).await;
    }
    // The watcher filters events like a scan does, so it follows the ignore
    // patterns and minimum size too.
    if old.watch_folders != new.watch_folders
        || old.ignore_patterns != new.ignore_patterns
        || old.min_file_size_mb != new.min_file_size_mb
    {
        watcher::sync(state).await;
    }
    // A work or output folder chosen anew (moved off a share, say) has its
    // drives and shares learned afresh, from now (it was just found
    // usable), and the one it replaces is forgotten. One saved again as it
    // was (or given another way that leads to the same place, through a
    // link say) keeps what was remembered for it: what is mounted there
    // now may be the bare mount point of a share that isn't connected, or
    // another drive in its place, which only the user's word takes as the
    // usual one (`relearn-mounts`).
    for (setting, before, after, same) in folders {
        if same {
            if let (Some(before), Some(after)) = (before, after)
                && before != after
            {
                share_mounts::forget_unless_used(state, Path::new(before)).await;
            }
            continue;
        }
        if let Some(before) = before {
            share_mounts::forget_unless_used(state, Path::new(before)).await;
        }
        if let Some(after) = after {
            let after = Path::new(after);
            // What is remembered for a folder also used for something else
            // stays (it is still that folder).
            if !share_mounts::used_besides(state, after, setting).await {
                share_mounts::forget(state, after).await;
            }
            match setting {
                FolderSetting::OutputFolder => share_mounts::check_output(state, after).await,
                FolderSetting::TempDir => share_mounts::check(state, after).await,
            };
        }
    }
    state.dispatcher.wake();
    state.broadcast_queue_state().await;
    Ok(new)
}

/// The output folder (in folder mode) and the work folder in use, with
/// whether the drives and shares they sit on are connected as they were.
/// Nothing is learned (but what is mounted at a place remembered without
/// it, see [`share_mounts::status`]), and no disk is touched.
pub async fn folder_statuses(state: &AppState) -> Vec<FolderStatus> {
    let mut out = Vec::new();
    for (setting, folder) in share_mounts::configured_folders(state) {
        let mounted = share_mounts::status(state, &folder).await;
        out.push(FolderStatus {
            setting,
            path: folder.display().to_string(),
            problem: mounted.problem(),
            changed_mount: match &mounted {
                Mounted::Different(point) => Some(point.display().to_string()),
                _ => None,
            },
        });
    }
    out
}

/// The user put another drive (or share) where one the output or work
/// folder was seen on, on purpose: take what is mounted there now as the
/// usual one (see [`share_mounts::relearn`]), also for the conversions
/// whose new file went there and isn't settled yet, so the jobs waiting for
/// it go on with it. A mount point with nothing mounted stays "not
/// connected". Answers `409 nothing_changed` when neither folder has
/// another drive in its share's place.
pub async fn relearn_folders(state: &AppState) -> ApiResult<Vec<FolderStatus>> {
    let mut taken: Vec<PathBuf> = Vec::new();
    for (setting, folder) in share_mounts::configured_folders(state) {
        let points = share_mounts::relearn(state, &folder)
            .await
            .map_err(ApiError::internal)?;
        if points.is_empty() {
            continue;
        }
        let what = match setting {
            FolderSetting::OutputFolder => "The output folder",
            FolderSetting::TempDir => "The work folder",
        };
        let places: Vec<String> = points.iter().map(|p| p.display().to_string()).collect();
        state
            .activity(
                ActivityLevel::Info,
                format!(
                    "{what} {} now uses the drive mounted at {}.",
                    folder.display(),
                    places.join(" and ")
                ),
                ActivityRefs::default(),
            )
            .await;
        taken.extend(points);
    }
    if taken.is_empty() {
        return Err(ApiError::conflict(
            "nothing_changed",
            "No other drive is mounted in place of the ones the output and work folders were on. \
             If one isn't connected, reconnect it.",
        ));
    }
    taken.sort();
    taken.dedup();
    share_mounts::relearn_placing_at(state, &taken)
        .await
        .map_err(ApiError::internal)?;
    // The jobs waiting for these folders are looked at again now.
    crate::services::dispatcher::recheck_all_offline(state).await;
    state.broadcast_queue_state().await;
    Ok(folder_statuses(state).await)
}

/// Build the settings to save on first run from the command line. `MAX_JOBS`
/// is not copied: it stands in for the automatic job count on every start
/// (see `dispatcher::effective_max_jobs`), so changing it later still works.
pub fn first_run_settings(config: &crate::config::Config) -> Settings {
    Settings {
        hardware: config.hw,
        ..Settings::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_replaces_top_level_keys() {
        let s = merge(
            &Settings::default(),
            json!({"max_jobs": 3, "auto_queue": false}),
        )
        .unwrap();
        assert_eq!(s.max_jobs, Some(3));
        assert!(!s.auto_queue);
        assert!(s.watch_folders);
    }

    #[test]
    fn merge_rejects_unknown_and_wrong_types() {
        let e = merge(&Settings::default(), json!({"colour": "gold"})).unwrap_err();
        assert_eq!(e.code, "unknown_setting");
        let e = merge(&Settings::default(), json!({"max_jobs": "lots"})).unwrap_err();
        assert_eq!(e.code, "invalid_settings");
        assert!(e.message.contains("max_jobs"), "{}", e.message);
        let e = merge(&Settings::default(), json!([1, 2])).unwrap_err();
        assert_eq!(e.code, "invalid_settings");
    }

    #[test]
    fn nested_mistakes_name_the_exact_field_in_plain_words() {
        let e = merge(
            &Settings::default(),
            json!({"default_profile": {"quality": "ultra"}}),
        )
        .unwrap_err();
        assert_eq!(e.field.as_deref(), Some("default_profile.quality"));
        assert!(e.message.contains("Choose one of: "), "{}", e.message);
        assert!(!e.message.contains('`'), "{}", e.message);
    }

    #[test]
    fn ignore_pattern_errors_are_plain_and_name_the_pattern() {
        let e = validate_ignore_pattern("Movies/[abc").unwrap_err();
        assert_eq!(
            ignore_pattern_error(&e),
            "The ignore pattern \"Movies/[abc\" has a [ without a closing ]."
        );
        // Whatever the glob library says is reduced to the plain reason.
        assert_eq!(
            ignore_pattern_error(
                "The ignore pattern \"[\" is not valid (error parsing glob '**/[': unclosed \
                 character class; missing ']'), so it was not used"
            ),
            "The ignore pattern \"[\" is not valid (unclosed character class; missing ']')."
        );
    }

    #[test]
    fn null_clears_optional_values() {
        let current = Settings {
            max_jobs: Some(4),
            ..Settings::default()
        };
        let s = merge(&current, json!({"max_jobs": null})).unwrap();
        assert_eq!(s.max_jobs, None);
    }
}
