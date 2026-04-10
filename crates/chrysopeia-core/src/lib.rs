//! Chrysopeia Core - shared types, configuration, and logic.

pub mod codec;
pub mod config;
pub mod error;
pub mod models;
pub mod profile;

pub use config::AppConfig;
pub use error::CoreError;
pub use models::*;
pub use profile::TranscodeProfile;
