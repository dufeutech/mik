//! Type definitions for process management.
//!
//! This module contains configuration and information types used across
//! the process management subsystem.

use std::path::PathBuf;

/// Default maximum log file size before rotation (10 MB).
pub const DEFAULT_MAX_LOG_SIZE: u64 = 10 * 1024 * 1024;

/// Default number of rotated log files to keep.
pub const DEFAULT_MAX_LOG_FILES: usize = 5;

/// Configuration for log rotation.
#[derive(Debug, Clone)]
pub struct LogRotationConfig {
    /// Maximum log file size in bytes before rotation.
    pub max_size: u64,
    /// Maximum number of rotated log files to keep.
    pub max_files: usize,
}

impl Default for LogRotationConfig {
    fn default() -> Self {
        Self {
            max_size: DEFAULT_MAX_LOG_SIZE,
            max_files: DEFAULT_MAX_LOG_FILES,
        }
    }
}

impl LogRotationConfig {
    /// Create a config with custom size limit in megabytes.
    ///
    /// # Note
    /// Provided for API completeness. Currently `spawn_instance` uses defaults.
    #[allow(dead_code)]
    pub fn with_size_mb(mb: u64) -> Self {
        Self {
            max_size: mb * 1024 * 1024,
            ..Default::default()
        }
    }

    /// Set the maximum number of rotated files to keep.
    ///
    /// # Note
    /// Provided for API completeness. Currently `spawn_instance` uses defaults.
    #[allow(dead_code)]
    #[must_use]
    pub const fn max_files(mut self, count: usize) -> Self {
        self.max_files = count;
        self
    }
}

/// Configuration for spawning a new mik instance.
#[derive(Debug, Clone, Default)]
pub struct SpawnConfig {
    /// Name of the instance (used for logging and tracking).
    pub name: String,
    /// Port to bind the HTTP server to.
    pub port: u16,
    /// Path to the mik.toml configuration file.
    pub config_path: PathBuf,
    /// Working directory for the process.
    pub working_dir: PathBuf,
    /// Enable hot-reload mode (bypasses AOT cache).
    pub hot_reload: bool,
}

/// Information about a running instance.
#[derive(Debug, Clone)]
pub struct InstanceInfo {
    /// Process ID of the running instance.
    pub pid: u32,
    /// Path to the log file.
    pub log_path: PathBuf,
}
