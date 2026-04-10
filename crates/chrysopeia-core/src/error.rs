//! Error types for chrysopeia-core.

use thiserror::Error;

/// Core error type for Chrysopeia operations.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Invalid codec: {0}")]
    InvalidCodec(String),

    #[error("Invalid container: {0}")]
    InvalidContainer(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Media probe failed: {0}")]
    ProbeFailed(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("{0}")]
    Other(String),
}
