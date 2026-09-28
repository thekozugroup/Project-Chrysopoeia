//! Settings changes: merge, validate, persist, and apply.

use std::path::Path;

use chrysopoeia_core::{Event, OutputMode, Settings};
use globset::Glob;
use serde_json::Value;

use crate::db;
use crate::error::{ApiError, ApiResult};
use crate::services::dispatcher::MAX_JOBS_LIMIT;
use crate::services::{hardware, watcher};
use crate::state::AppState;

fn invalid(message: impl Into<String>) -> ApiError {
    ApiError::bad_request("invalid_settings", message)
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
            ));
        }
    }
    let keys: Vec<String> = patch.keys().cloned().collect();
    for (k, v) in patch {
        merged.insert(k, v);
    }
    serde_json::from_value::<Settings>(Value::Object(merged.clone())).map_err(|e| {
        // Name the offending setting when a single key explains the error.
        let culprit = keys.iter().find(|k| {
            let Ok(Value::Object(mut probe)) = serde_json::to_value(current) else {
                return false;
            };
            if let Some(v) = merged.get(*k) {
                probe.insert((*k).clone(), v.clone());
            }
            serde_json::from_value::<Settings>(Value::Object(probe)).is_err()
        });
        match culprit {
            Some(k) => invalid(format!("The value for \"{k}\" isn't valid: {e}")),
            None => invalid(format!("The settings aren't valid: {e}")),
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

/// Check that `dir` is an existing, writable folder.
async fn check_writable_dir(dir: &str, what: &str) -> ApiResult<()> {
    let path = Path::new(dir);
    if !path.is_absolute() {
        return Err(invalid(format!(
            "The {what} must be a full path starting with /."
        )));
    }
    match tokio::fs::metadata(path).await {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            return Err(invalid(format!(
                "The {what} {dir} is a file, not a folder."
            )));
        }
        Err(_) => {
            return Err(invalid(format!(
                "The {what} {dir} doesn't exist on the server. In Docker, check that it is mounted."
            )));
        }
    }
    let probe = path.join(format!(
        ".chrysopoeia-write-check-{}",
        uuid::Uuid::new_v4().simple()
    ));
    match tokio::fs::write(&probe, b"ok").await {
        Ok(()) => {
            let _ = tokio::fs::remove_file(&probe).await;
            Ok(())
        }
        Err(_) => Err(invalid(format!(
            "Chrysopoeia can't write to the {what} {dir}. Check its permissions (in Docker, the \
             PUID/PGID user needs write access)."
        ))),
    }
}

/// Normalize and validate new settings.
pub async fn validate(state: &AppState, s: &mut Settings) -> ApiResult<()> {
    blank_to_none(&mut s.temp_dir);
    blank_to_none(&mut s.output_folder);
    s.ignore_patterns = s
        .ignore_patterns
        .iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    s.default_profile.normalize();

    if let Some(n) = s.max_jobs
        && !(1..=MAX_JOBS_LIMIT).contains(&n)
    {
        return Err(invalid(format!(
            "Jobs at once must be between 1 and {MAX_JOBS_LIMIT}."
        )));
    }
    if let Some(h) = s.active_hours
        && (h.start > 23 || h.end > 23)
    {
        return Err(invalid("Active hours must be whole hours from 0 to 23."));
    }
    for p in &s.ignore_patterns {
        if let Err(e) = Glob::new(p) {
            return Err(invalid(format!(
                "\"{p}\" isn't a valid ignore pattern: {e}"
            )));
        }
    }
    if let Some(dir) = s.temp_dir.clone() {
        check_writable_dir(&dir, "temporary folder").await?;
    }
    if s.output_mode == OutputMode::Folder {
        let Some(dir) = s.output_folder.clone() else {
            return Err(invalid(
                "Choose an output folder, or switch back to replacing the originals.",
            ));
        };
        check_writable_dir(&dir, "output folder").await?;
        let out = tokio::fs::canonicalize(&dir)
            .await
            .unwrap_or_else(|_| Path::new(&dir).to_path_buf());
        for lib in db::libraries::list(state.db.pool()).await? {
            if out.starts_with(&lib.path) {
                return Err(invalid(format!(
                    "The output folder can't be inside the library {}, or Chrysopoeia would \
                     convert its own results.",
                    lib.name
                )));
            }
        }
    }
    Ok(())
}

/// Apply a partial update: merge, validate, persist, publish and apply.
pub async fn patch(state: &AppState, patch: Value) -> ApiResult<Settings> {
    let _guard = state.settings_write.lock().await;
    let old = state.settings();
    let mut new = merge(&old, patch)?;
    validate(state, &mut new).await?;
    db::settings::save(state.db.pool(), &new).await?;
    state.replace_settings(new.clone());
    state.emit(Event::SettingsUpdated {
        settings: Box::new(new.clone()),
    });

    if old.hardware != new.hardware {
        hardware::apply_preference(state, new.hardware).await;
    }
    if old.watch_folders != new.watch_folders {
        watcher::sync(state).await;
    }
    state.dispatcher.wake();
    state.broadcast_queue_state().await;
    Ok(new)
}

/// Build the settings to save on first run from the command line.
pub fn first_run_settings(config: &crate::config::Config) -> Settings {
    Settings {
        hardware: config.hw,
        max_jobs: config.max_jobs,
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
    fn null_clears_optional_values() {
        let current = Settings {
            max_jobs: Some(4),
            ..Settings::default()
        };
        let s = merge(&current, json!({"max_jobs": null})).unwrap();
        assert_eq!(s.max_jobs, None);
    }
}
