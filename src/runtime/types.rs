//! Common types for the runtime module.
//!
//! This module contains data types used across the runtime for health checks,
//! error categorization, and memory statistics.

use std::fmt;

use serde::Serialize;

/// Error category for observability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// Module not found or failed to load.
    ModuleLoad,
    /// Invalid request (routing, path, etc).
    InvalidRequest,
    /// Component instantiation failed.
    Instantiation,
    /// Handler execution failed.
    Execution,
    /// Static file serving error.
    StaticFile,
    /// Execution timeout.
    Timeout,
    /// Script execution error.
    Script,
    /// Reliability error (circuit breaker, rate limit).
    Reliability,
    /// Internal server error.
    Internal,
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ModuleLoad => write!(f, "module_load"),
            Self::InvalidRequest => write!(f, "invalid_request"),
            Self::Instantiation => write!(f, "instantiation"),
            Self::Execution => write!(f, "execution"),
            Self::StaticFile => write!(f, "static_file"),
            Self::Timeout => write!(f, "timeout"),
            Self::Script => write!(f, "script"),
            Self::Reliability => write!(f, "reliability"),
            Self::Internal => write!(f, "internal"),
        }
    }
}

/// Level of detail for health check responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HealthDetail {
    /// Summary only - minimal allocation, fast response.
    #[default]
    Summary,
    /// Full details including list of loaded modules.
    Full,
}

/// Health status response.
#[derive(Debug, Serialize)]
pub struct HealthStatus {
    /// Overall status.
    pub status: String,
    /// Timestamp of the health check.
    pub timestamp: String,
    /// Number of modules currently in cache.
    pub cache_size: usize,
    /// Maximum cache capacity (entries).
    pub cache_capacity: usize,
    /// Current cache memory usage (bytes).
    pub cache_bytes: usize,
    /// Maximum cache memory (bytes).
    pub cache_max_bytes: usize,
    /// Total requests handled.
    pub total_requests: u64,
    /// Memory statistics.
    pub memory: MemoryStats,
    /// List of loaded modules (optional, only included with ?verbose=true).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loaded_modules: Option<Vec<String>>,
}

/// Memory statistics for health check.
#[derive(Debug, Serialize)]
pub struct MemoryStats {
    /// Allocated memory (if available).
    pub allocated_bytes: Option<usize>,
    /// Memory limit per request.
    pub limit_per_request_bytes: usize,
}
