//! Transcoding worker for Chrysopeia.
//!
//! Processes transcode jobs using either native oximedia or FFmpeg backends.

pub mod engine;
pub mod ffmpeg_backend;
pub mod oximedia_backend;
pub mod progress;
pub mod strategy;

pub use engine::TranscodeEngine;
