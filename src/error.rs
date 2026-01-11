//! Error types for Neuraphage.

use thiserror::Error;

use crate::TaskStatus;

/// Neuraphage error type.
#[derive(Error, Debug)]
pub enum Error {
    /// Task not found
    #[error("task not found: {id}")]
    TaskNotFound { id: String },

    /// Invalid task state transition
    #[error("invalid state transition: {from:?} -> {to:?}")]
    InvalidStateTransition { from: TaskStatus, to: TaskStatus },

    /// Engram storage error
    #[error("storage error: {0}")]
    Storage(#[from] engram::StoreError),

    /// Engram eyre error (from engram operations)
    #[error("engram error: {0}")]
    Engram(String),

    /// Configuration error
    #[error("configuration error: {0}")]
    Config(String),

    /// IO error
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization error
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// YAML serialization error
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// Daemon error
    #[error("daemon error: {0}")]
    Daemon(String),

    /// Task validation error
    #[error("validation error: {0}")]
    Validation(String),
}

impl From<eyre::Report> for Error {
    fn from(e: eyre::Report) -> Self {
        Error::Engram(e.to_string())
    }
}

/// Result type alias for Neuraphage.
pub type Result<T> = std::result::Result<T, Error>;
