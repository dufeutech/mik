//! Process management for mik daemon instances.
//!
//! This module handles spawning and managing mik server instances as background
//! processes. Each instance runs independently with its own port, logs, and
//! configuration.
//!
//! ## Log Rotation
//!
//! Log files are automatically rotated when they exceed a configured size.
//! Rotated logs are renamed with a timestamp suffix (e.g., `default.log.20250101-120000`).
//!
//! ## Module Structure
//!
//! - [`types`]: Configuration and information types
//! - [`lifecycle`]: Process spawning and termination
//! - [`health`]: Health checking and log reading
//! - [`log_rotation`]: Log file rotation
//! - [`utils`]: Shared utility functions

mod health;
mod lifecycle;
mod log_rotation;
mod types;
mod utils;

// Re-export public API
// Process management functions are used by HTTP handlers for instance lifecycle.
pub use health::{is_running, tail_log};
pub use lifecycle::{kill_instance, spawn_instance};
pub use types::SpawnConfig;
