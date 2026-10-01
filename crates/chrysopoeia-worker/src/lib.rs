//! Transcode execution: decide what to do with a file, build the ffmpeg
//! command, run it with a hardware-to-software fallback chain, verify the
//! result, and put it in place safely.
//!
//! Module ownership (see docs/ARCHITECTURE.md):
//! - `plan`, `quality` — pure planning: skip decisions and ffmpeg arguments.
//! - `ffmpeg`, `run`, `validate`, `finalize` — processes, verification, files.

pub mod ffmpeg;
pub mod finalize;
pub mod plan;
pub mod quality;
pub mod run;
pub mod validate;

pub use plan::{
    CoverFile, Decision, FfmpegPlan, PlanRequest, StreamSummary, build_plan, cover_extract_args,
    decide, decide_forced, replace_loss,
};
pub use run::{JobOutcome, JobSpec, RunConfig, run_job};
pub use validate::{ValidateRequest, validate_output};
