//! Output verification: structure, full decode, and visual comparison
//! against the source. Implemented by the worker-run agent.

use std::path::Path;

use chrysopoeia_core::{ProbeInfo, TranscodeProfile, ValidationLevel, ValidationReport};
use tokio_util::sync::CancellationToken;

use crate::plan::StreamSummary;

/// Inputs for [`validate_output`].
#[derive(Debug, Clone, Copy)]
pub struct ValidateRequest<'a> {
    pub ffmpeg: &'a Path,
    pub ffprobe: &'a Path,
    pub source: &'a Path,
    pub source_probe: &'a ProbeInfo,
    pub output: &'a Path,
    pub profile: &'a TranscodeProfile,
    pub level: ValidationLevel,
    pub expected: StreamSummary,
}

/// Verify an encoded file. `on_progress` receives 0..=100.
pub async fn validate_output(
    req: &ValidateRequest<'_>,
    cancel: &CancellationToken,
    on_progress: &(dyn Fn(f32) + Send + Sync),
) -> ValidationReport {
    let _ = (req, cancel, on_progress);
    todo!("implemented by the worker-run agent")
}
