//! Output verification results.

use serde::{Deserialize, Serialize};

use crate::settings::ValidationLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    /// Suspicious but not disqualifying (e.g. a dropped image subtitle).
    Warn,
    Fail,
    Skipped,
}

/// One verification step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationCheck {
    /// Stable id: `probe`, `streams`, `duration`, `decode`, `visual`,
    /// `black_frames`, `frozen_frames`, `size`.
    pub id: String,
    /// Short human label, e.g. "Plays start to finish".
    pub label: String,
    pub status: CheckStatus,
    /// One sentence of detail, e.g. "Duration matches (1:42:10 vs 1:42:10)".
    pub detail: String,
    /// Main measured value for the check, if numeric (SSIM, seconds, ...).
    pub value: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    /// True when no check failed.
    pub passed: bool,
    pub level: ValidationLevel,
    pub checks: Vec<ValidationCheck>,
    /// Lowest SSIM (0..1) across sampled segments.
    pub ssim_min: Option<f64>,
    /// Mean SSIM across sampled segments.
    pub ssim_avg: Option<f64>,
    /// Mean PSNR in dB across sampled segments.
    pub psnr_avg: Option<f64>,
    pub elapsed_secs: f64,
}

impl ValidationReport {
    /// First failing check, for error messages.
    pub fn first_failure(&self) -> Option<&ValidationCheck> {
        self.checks.iter().find(|c| c.status == CheckStatus::Fail)
    }
}
