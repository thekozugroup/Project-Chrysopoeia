//! Global settings, persisted by the server and edited in the UI.

use serde::{Deserialize, Serialize};

use crate::hardware::HwPreference;
use crate::profile::TranscodeProfile;

/// Where finished files go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// Replace the original file once the new one passes verification.
    Replace,
    /// Write into `output_folder`, mirroring the library's folder structure,
    /// and leave originals untouched.
    Folder,
}

/// How hard to check each finished file before it replaces the original.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationLevel {
    /// No checks. Not recommended.
    Off,
    /// Container, stream and duration checks only.
    Quick,
    /// Quick + full decode of the output + visual comparison at 4 points.
    Standard,
    /// Standard + visual comparison at 10 points + black/frozen frame checks.
    Thorough,
}

/// Daily window during which new jobs may start (local time, 24h clock).
/// `start == end` means all day. `start > end` wraps past midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveHours {
    pub start: u8,
    pub end: u8,
}

impl ActiveHours {
    pub fn contains(self, hour: u8) -> bool {
        let (s, e, h) = (self.start % 24, self.end % 24, hour % 24);
        if s == e {
            true
        } else if s < e {
            h >= s && h < e
        } else {
            h >= s || h < e
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Queue files that need work automatically after each scan.
    pub auto_queue: bool,
    /// Pick up new and changed files as they appear.
    pub watch_folders: bool,
    /// Full rescan interval in hours. 0 disables periodic rescans.
    pub rescan_interval_hours: u32,
    /// Concurrent jobs. `None` = automatic, based on detected hardware.
    pub max_jobs: Option<u32>,
    pub hardware: HwPreference,
    /// If a hardware encode fails, retry on the CPU.
    pub cpu_fallback: bool,
    pub validation: ValidationLevel,
    pub output_mode: OutputMode,
    /// Required when `output_mode` is `Folder`.
    pub output_folder: Option<String>,
    /// Scratch space for in-progress encodes. `None` writes next to the source.
    pub temp_dir: Option<String>,
    /// Copy the original's modification time onto the new file, so media
    /// servers don't list it as newly added.
    pub keep_file_dates: bool,
    /// Run ffmpeg at reduced CPU priority.
    pub low_priority: bool,
    /// Only start new jobs inside this window. `None` = any time.
    pub active_hours: Option<ActiveHours>,
    /// Glob patterns (relative to a library root) to ignore, e.g. `**/Extras/**`.
    pub ignore_patterns: Vec<String>,
    /// Ignore files smaller than this many megabytes (samples, trailers).
    pub min_file_size_mb: u32,
    /// Profile given to newly added libraries.
    pub default_profile: TranscodeProfile,
    /// Set once the first-run setup has been completed or dismissed.
    pub onboarded: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            auto_queue: true,
            watch_folders: true,
            rescan_interval_hours: 12,
            max_jobs: None,
            hardware: HwPreference::Auto,
            cpu_fallback: true,
            validation: ValidationLevel::Standard,
            output_mode: OutputMode::Replace,
            output_folder: None,
            temp_dir: None,
            keep_file_dates: true,
            low_priority: true,
            active_hours: None,
            ignore_patterns: vec![
                "**/.*".into(),
                "**/@eaDir/**".into(),
                "**/#recycle/**".into(),
                "**/*.partial~".into(),
            ],
            min_file_size_mb: 0,
            default_profile: TranscodeProfile::default(),
            onboarded: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_hours_wrap_midnight() {
        let night = ActiveHours { start: 22, end: 6 };
        assert!(night.contains(23));
        assert!(night.contains(2));
        assert!(!night.contains(12));
        let all = ActiveHours { start: 0, end: 0 };
        assert!(all.contains(15));
    }

    #[test]
    fn partial_json_fills_defaults() {
        let s: Settings = serde_json::from_str(r#"{"max_jobs":3}"#).unwrap();
        assert_eq!(s.max_jobs, Some(3));
        assert!(s.auto_queue);
        assert_eq!(s.validation, ValidationLevel::Standard);
    }
}
