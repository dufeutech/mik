//! Centralized constants for security limits and defaults.
//!
//! All magic numbers in the runtime should be defined here with
//! documented rationale. This enables:
//! - Security auditing in one place
//! - Consistent limits across modules
//! - Easy tuning without code search

// =============================================================================
// Security Limits
// =============================================================================

/// Maximum request body size (10 MB).
/// Prevents memory exhaustion from large uploads.
pub const MAX_BODY_SIZE_BYTES: usize = 10 * 1024 * 1024;

/// Maximum path length for security validation (4096 bytes).
pub const MAX_PATH_LENGTH: usize = 4096;

/// Maximum WASM execution time (30 seconds).
pub const MAX_WASM_TIMEOUT_SECS: u64 = 30;

/// Default fuel budget per request (1 billion operations).
/// Fuel provides deterministic CPU limiting complementing epoch-based preemption.
/// Trade-off: ~10-20% overhead but guarantees deterministic execution limits.
pub const DEFAULT_FUEL_BUDGET: u64 = 1_000_000_000;

/// Minimum size for gzip compression (1 KB).
/// Smaller responses don't benefit from compression overhead.
pub const GZIP_MIN_SIZE: usize = 1024;

// =============================================================================
// Reliability Defaults
// =============================================================================

/// Default cache size (number of modules).
pub const DEFAULT_CACHE_SIZE: usize = 100;

/// Default cache memory limit (256 MB).
pub const DEFAULT_CACHE_MB: usize = 256;

/// Default concurrent request limit (global).
pub const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 1000;

/// Default per-module concurrent request limit.
pub const DEFAULT_MAX_PER_MODULE_REQUESTS: usize = 10;

/// Circuit breaker failure threshold before opening.
/// Rationale: 5 consecutive failures indicates a real problem, not transient.
pub const CIRCUIT_BREAKER_FAILURE_THRESHOLD: u32 = 5;

/// Circuit breaker success threshold before closing from half-open.
/// Rationale: 2 successes confirms the service has recovered.
pub const CIRCUIT_BREAKER_SUCCESS_THRESHOLD: u32 = 2;

/// Circuit breaker timeout before transitioning to half-open (30 seconds).
/// Rationale: Long enough to allow transient issues to resolve.
pub const CIRCUIT_BREAKER_TIMEOUT_SECS: u64 = 30;

/// Circuit breaker recovery timeout (60 seconds).
/// Rationale: Matches typical service restart time.
pub const CIRCUIT_BREAKER_RECOVERY_SECS: u64 = 60;

/// Default server port.
pub const DEFAULT_PORT: u16 = 3000;

// =============================================================================
// Paths
// =============================================================================

/// Default modules directory.
pub const DEFAULT_MODULES_DIR: &str = "modules";

// =============================================================================
// Health Check
// =============================================================================

/// Health check status indicating the service is ready.
pub const HEALTH_STATUS_READY: &str = "ready";

// =============================================================================
// AOT Cache
// =============================================================================

/// Default AOT cache time-to-idle in seconds (1 hour).
/// Entries not accessed within this time are evicted.
pub const DEFAULT_AOT_CACHE_TTI_SECS: u64 = 3600;

/// Default AOT cache size in bytes (1 GB).
/// Maximum memory used by the AOT compiled module cache.
pub const DEFAULT_AOT_CACHE_SIZE_BYTES: u64 = 1024 * 1024 * 1024;
