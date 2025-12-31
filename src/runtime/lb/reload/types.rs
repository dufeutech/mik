//! Core types for graceful reload operations.

use std::time::{Duration, Instant};

use super::super::backend::Backend;

/// Configuration for graceful reload operations.
#[derive(Debug, Clone)]
pub struct ReloadConfig {
    /// Maximum time to wait for a backend to drain requests before forcefully removing it.
    /// Default is 30 seconds.
    pub drain_timeout: Duration,
}

impl Default for ReloadConfig {
    fn default() -> Self {
        Self {
            drain_timeout: Duration::from_secs(30),
        }
    }
}

impl ReloadConfig {
    /// Create a new reload configuration with custom drain timeout.
    #[allow(dead_code)]
    pub const fn with_drain_timeout(drain_timeout: Duration) -> Self {
        Self { drain_timeout }
    }
}

/// Signal payload for reload operations.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ReloadSignal {
    /// New list of backend addresses.
    pub backends: Vec<String>,
    /// Timestamp when the reload was requested.
    pub requested_at: Instant,
}

impl ReloadSignal {
    /// Create a new reload signal with the given backend addresses.
    pub fn new(backends: Vec<String>) -> Self {
        Self {
            backends,
            requested_at: Instant::now(),
        }
    }
}

/// Result of a reload operation.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ReloadResult {
    /// Backend addresses that were added.
    pub added: Vec<String>,
    /// Backend addresses that are being drained.
    pub draining: Vec<String>,
    /// Backend addresses that remained unchanged.
    pub unchanged: Vec<String>,
}

impl ReloadResult {
    /// Check if any changes were made.
    #[allow(dead_code)]
    pub const fn has_changes(&self) -> bool {
        !self.added.is_empty() || !self.draining.is_empty()
    }
}

/// A backend that is being drained.
#[derive(Debug, Clone)]
pub(crate) struct DrainingBackend {
    /// The backend being drained.
    pub backend: Backend,
    /// When the drain started.
    pub started_at: Instant,
}
