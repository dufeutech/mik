//! Daemon error types for the mik background service.
//!
//! This module provides structured errors for daemon operations,
//! with HTTP status code mappings for API responses.

// Allow unused - typed errors defined for gradual migration from anyhow
#![allow(dead_code)]

/// Result type for daemon operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Daemon errors with structured context.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Process not found.
    #[error("process not found: {name}")]
    ProcessNotFound { name: String },

    /// Process already running.
    #[error("process already running: {name}")]
    ProcessAlreadyRunning { name: String },

    /// Process failed to start.
    #[error("failed to start process '{name}': {reason}")]
    ProcessStartFailed { name: String, reason: String },

    /// Process failed to stop.
    #[error("failed to stop process '{name}': {reason}")]
    ProcessStopFailed { name: String, reason: String },

    /// Configuration error.
    #[error("configuration error: {0}")]
    Config(String),

    /// IO error with context.
    #[error("IO error in {context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },

    /// State file error.
    #[error("state file error: {0}")]
    StateFile(String),

    /// Watch error.
    #[error("file watch error: {0}")]
    Watch(String),

    /// Cron error.
    #[error("cron error: {0}")]
    Cron(String),

    /// Service error.
    #[error("service error: {0}")]
    Service(String),

    /// Invalid request.
    #[error("invalid request: {0}")]
    InvalidRequest(String),

    /// Internal error.
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// Create an IO error with context.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    /// Create a process not found error.
    pub fn process_not_found(name: impl Into<String>) -> Self {
        Self::ProcessNotFound { name: name.into() }
    }

    /// Create a process already running error.
    pub fn process_already_running(name: impl Into<String>) -> Self {
        Self::ProcessAlreadyRunning { name: name.into() }
    }

    /// Create a process start failed error.
    pub fn process_start_failed(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::ProcessStartFailed {
            name: name.into(),
            reason: reason.into(),
        }
    }

    /// Get the appropriate HTTP status code for this error.
    pub const fn status_code(&self) -> u16 {
        match self {
            Self::ProcessNotFound { .. } => 404,
            Self::ProcessAlreadyRunning { .. } => 409,
            Self::InvalidRequest(_) => 400,
            Self::Config(_) => 422,
            Self::ProcessStartFailed { .. }
            | Self::ProcessStopFailed { .. }
            | Self::Io { .. }
            | Self::StateFile(_)
            | Self::Watch(_)
            | Self::Cron(_)
            | Self::Service(_)
            | Self::Internal(_) => 500,
        }
    }

    /// Get a client-safe error message (doesn't leak internal details).
    pub const fn client_message(&self) -> &str {
        match self {
            Self::ProcessNotFound { .. } => "Process not found",
            Self::ProcessAlreadyRunning { .. } => "Process already running",
            Self::InvalidRequest(_) => "Invalid request",
            Self::Config(_) => "Configuration error",
            _ => "Internal server error",
        }
    }
}
