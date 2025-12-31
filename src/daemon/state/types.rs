//! Type definitions for persistent state management.
//!
//! Contains the core data structures used by the state store:
//! - `Status` - Runtime status of WASM instances
//! - `Instance` - WASM instance metadata

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Runtime status of a WASM instance
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Status {
    /// Instance is currently running
    Running,
    /// Instance was stopped gracefully
    Stopped,
    /// Instance crashed with exit code
    Crashed { exit_code: i32 },
}

/// Metadata for a WASM instance managed by the daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    /// Unique instance name (e.g., "default", "staging")
    pub name: String,
    /// HTTP port the instance listens on
    pub port: u16,
    /// Operating system process ID
    pub pid: u32,
    /// Current runtime status
    pub status: Status,
    /// Path to the mik.toml configuration file used
    pub config: PathBuf,
    /// Timestamp when instance was started
    pub started_at: DateTime<Utc>,
    /// List of loaded WASM modules (relative paths from config)
    pub modules: Vec<String>,
    /// Whether auto-restart on crash is enabled (default: false)
    #[serde(default)]
    pub auto_restart: bool,
    /// Number of times this instance has been auto-restarted
    #[serde(default)]
    pub restart_count: u32,
    /// Timestamp of last auto-restart (for backoff calculation)
    #[serde(default)]
    pub last_restart_at: Option<DateTime<Utc>>,
}
