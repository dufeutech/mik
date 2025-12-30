//! Circuit breaker configuration.
//!
//! Defines thresholds, timeouts, and cache limits for the circuit breaker.

use crate::constants;
use std::time::Duration;

/// Circuit breaker configuration.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures before opening circuit.
    pub failure_threshold: u32,
    /// Duration to wait before attempting recovery.
    pub timeout: Duration,
    /// Maximum time a probe can be in flight before timing out.
    pub probe_timeout: Duration,
    /// Maximum number of keys to track (LRU eviction).
    pub max_tracked_keys: usize,
    /// Time after which idle keys are evicted.
    pub idle_timeout: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: constants::CIRCUIT_BREAKER_FAILURE_THRESHOLD,
            timeout: Duration::from_secs(constants::CIRCUIT_BREAKER_RECOVERY_SECS),
            probe_timeout: Duration::from_secs(constants::CIRCUIT_BREAKER_RECOVERY_SECS),
            max_tracked_keys: 1000,
            idle_timeout: Duration::from_secs(600),
        }
    }
}
