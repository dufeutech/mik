//! Type definitions for persistent state management.
//!
//! Contains the core data structures used by the state store:
//! - `ServiceType` - Sidecar service classification
//! - `Sidecar` - Registered sidecar service metadata
//! - `Status` - Runtime status of WASM instances
//! - `Instance` - WASM instance metadata

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Type of sidecar service
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ServiceType {
    /// Key-value store (Redis-like)
    Kv,
    /// SQL database (SQLite/Postgres)
    Sql,
    /// Object storage (S3-like)
    Storage,
    /// Message queue
    Queue,
    /// Custom service type
    Custom(String),
}

impl std::fmt::Display for ServiceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kv => write!(f, "kv"),
            Self::Sql => write!(f, "sql"),
            Self::Storage => write!(f, "storage"),
            Self::Queue => write!(f, "queue"),
            Self::Custom(name) => write!(f, "custom:{name}"),
        }
    }
}

/// Metadata for a registered sidecar service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sidecar {
    /// Unique service name (e.g., "mikcar-primary", "redis-cache")
    pub name: String,
    /// Type of service provided
    pub service_type: ServiceType,
    /// Base URL for the service (e.g., `http://localhost:9001`)
    pub url: String,
    /// Optional description
    #[serde(default)]
    pub description: Option<String>,
    /// Timestamp when service was registered
    pub registered_at: DateTime<Utc>,
    /// Last heartbeat timestamp (updated by health checks)
    pub last_heartbeat: DateTime<Utc>,
    /// Whether the service is currently healthy
    #[serde(default = "default_healthy")]
    pub healthy: bool,
}

const fn default_healthy() -> bool {
    true
}

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
