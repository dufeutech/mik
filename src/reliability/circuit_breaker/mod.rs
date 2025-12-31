//! Circuit breaker with atomic state transitions.
//!
//! This implementation uses atomic check-and-modify operations to ensure
//! thread-safety under concurrent load. All state transitions are performed
//! atomically using moka's entry API.
//!
//! ## States
//!
//! - **Closed**: Normal operation, requests allowed
//! - **Open**: Too many failures, requests rejected
//! - **`HalfOpen`**: Testing recovery - only ONE probe request allowed
//!
//! ## Usage
//!
//! ```rust,ignore
//! use mik::reliability::{CircuitBreaker, CircuitBreakerConfig};
//! use std::time::Duration;
//!
//! let cb = CircuitBreaker::with_config(CircuitBreakerConfig {
//!     failure_threshold: 3,
//!     timeout: Duration::from_secs(30),
//!     ..Default::default()
//! });
//!
//! // Check before calling
//! if cb.check_request("database").is_ok() {
//!     // Make the call...
//!     cb.record_success("database");
//! }
//! ```

mod config;
mod error;
mod state;

#[cfg(test)]
mod tests;

// Re-export public types for backward compatibility
pub use config::CircuitBreakerConfig;
pub use error::{CircuitOpenError, CircuitOpenReason};
pub use state::CircuitState;

use moka::ops::compute::Op;
use moka::sync::Cache as MokaCache;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, warn};

/// Per-key circuit breaker with LRU eviction.
///
/// Thread-safe and suitable for concurrent use. All state transitions
/// are performed atomically to prevent race conditions.
#[derive(Clone)]
pub struct CircuitBreaker {
    states: MokaCache<Arc<str>, CircuitState>,
    config: CircuitBreakerConfig,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with default configuration.
    pub fn new() -> Self {
        Self::with_config(CircuitBreakerConfig::default())
    }

    /// Create a new circuit breaker with custom configuration.
    pub fn with_config(config: CircuitBreakerConfig) -> Self {
        let states = MokaCache::builder()
            .max_capacity(config.max_tracked_keys as u64)
            .time_to_idle(config.idle_timeout)
            .build();

        Self { states, config }
    }

    /// Check if a request should be allowed.
    ///
    /// Returns `Ok(())` if allowed, `Err(CircuitOpenError)` if circuit is open.
    /// State transitions (`Open` -> `HalfOpen`) are performed atomically.
    ///
    /// In `HalfOpen` state, only ONE probe request is allowed. Subsequent requests
    /// are rejected until the probe completes (via `record_success` or `record_failure`).
    pub fn check_request(&self, key: &str) -> Result<(), CircuitOpenError> {
        let timeout = self.config.timeout;
        let probe_timeout = self.config.probe_timeout;

        // Use atomic compute to check and potentially transition state
        let mut allowed = true;
        let mut error_info: Option<(u32, CircuitOpenReason)> = None;

        let cache_key: Arc<str> = Arc::from(key);
        self.states
            .entry_by_ref(&cache_key)
            .and_compute_with(|entry| {
                entry.map_or(Op::Nop, |entry| {
                    let state = entry.into_value();
                    match state {
                        CircuitState::Closed { .. } => {
                            // Closed - allow request, no state change
                            Op::Nop
                        },
                        CircuitState::Open {
                            opened_at,
                            failure_count,
                        } => {
                            if opened_at.elapsed() >= timeout {
                                // Timeout elapsed - transition to HalfOpen
                                // This request becomes the probe
                                info!("Circuit breaker for '{}' transitioning to half-open", key);
                                Op::Put(CircuitState::HalfOpen {
                                    started_at: Instant::now(),
                                })
                            } else {
                                // Still within timeout - reject
                                allowed = false;
                                error_info = Some((failure_count, CircuitOpenReason::Open));
                                Op::Nop
                            }
                        },
                        CircuitState::HalfOpen { started_at } => {
                            if started_at.elapsed() >= probe_timeout {
                                // Probe timed out - allow new probe
                                warn!(
                                    "Circuit breaker for '{}' probe timed out, allowing new probe",
                                    key
                                );
                                Op::Put(CircuitState::HalfOpen {
                                    started_at: Instant::now(),
                                })
                            } else {
                                // Probe still in flight - reject
                                allowed = false;
                                error_info = Some((0, CircuitOpenReason::ProbeInFlight));
                                Op::Nop
                            }
                        },
                    }
                })
            });

        if allowed {
            Ok(())
        } else {
            let (failure_count, reason) = error_info.unwrap_or((0, CircuitOpenReason::Open));
            Err(CircuitOpenError {
                key: key.to_string(),
                failure_count,
                reason,
            })
        }
    }

    /// Check if circuit is currently blocking requests.
    ///
    /// Returns true if the circuit is Open (and timeout not elapsed) or
    /// `HalfOpen` with a probe in flight.
    #[allow(dead_code)] // Inspection method for debugging/monitoring
    pub fn is_blocking(&self, key: &str) -> bool {
        self.check_request(key).is_err()
    }

    /// Check if circuit is in Open state (without considering timeout).
    #[allow(dead_code)] // Inspection method for debugging/monitoring
    pub fn is_open(&self, key: &str) -> bool {
        matches!(self.states.get(key), Some(CircuitState::Open { .. }))
    }

    /// Record a successful request.
    ///
    /// Atomically transitions the circuit based on current state:
    /// - Closed: Resets failure count to 0
    /// - `HalfOpen`: Transitions to `Closed` (recovery successful)
    /// - Open: Logs warning (unexpected state)
    pub fn record_success(&self, key: &str) {
        let cache_key: Arc<str> = Arc::from(key);

        self.states
            .entry_by_ref(&cache_key)
            .and_compute_with(|entry| {
                entry.map_or(Op::Nop, |entry| {
                    let state = entry.into_value();
                    match state {
                        CircuitState::Closed { failure_count: 0 } => {
                            // Already at zero failures - no update needed
                            Op::Nop
                        },
                        CircuitState::Closed { .. } => {
                            // Reset failure count
                            Op::Put(CircuitState::Closed { failure_count: 0 })
                        },
                        CircuitState::HalfOpen { .. } => {
                            // Recovery successful - close circuit
                            info!(
                                "Circuit breaker for '{}' closing after successful recovery",
                                key
                            );
                            Op::Put(CircuitState::Closed { failure_count: 0 })
                        },
                        CircuitState::Open { .. } => {
                            // Unexpected - shouldn't get success in open state
                            warn!("Unexpected success in open circuit state for '{}'", key);
                            Op::Nop
                        },
                    }
                })
            });
    }

    /// Record a failed request.
    ///
    /// Atomically updates the circuit state based on current state:
    /// - Closed: Increments failure count, opens if threshold reached
    /// - `HalfOpen`: Reopens the circuit (recovery failed)
    /// - Open: Extends the open period
    pub fn record_failure(&self, key: &str) {
        let threshold = self.config.failure_threshold;
        let cache_key: Arc<str> = Arc::from(key);

        self.states
            .entry_by_ref(&cache_key)
            .and_compute_with(|entry| {
                entry.map_or_else(
                    || {
                        // New key - start counting failures
                        // Check if threshold=1 means we should open immediately
                        if threshold <= 1 {
                            warn!("Circuit breaker opening for '{}' after 1 failure", key);
                            Op::Put(CircuitState::Open {
                                opened_at: Instant::now(),
                                failure_count: 1,
                            })
                        } else {
                            Op::Put(CircuitState::Closed { failure_count: 1 })
                        }
                    },
                    |entry| {
                        let state = entry.into_value();
                        match state {
                            CircuitState::Closed { failure_count } => {
                                let new_count = failure_count.saturating_add(1);
                                if new_count >= threshold {
                                    warn!(
                                        "Circuit breaker opening for '{}' after {} failures",
                                        key, new_count
                                    );
                                    Op::Put(CircuitState::Open {
                                        opened_at: Instant::now(),
                                        failure_count: new_count,
                                    })
                                } else {
                                    Op::Put(CircuitState::Closed {
                                        failure_count: new_count,
                                    })
                                }
                            },
                            CircuitState::HalfOpen { .. } => {
                                // Recovery failed - reopen circuit
                                warn!(
                                    "Circuit breaker for '{}' reopening after failed recovery",
                                    key
                                );
                                Op::Put(CircuitState::Open {
                                    opened_at: Instant::now(),
                                    failure_count: 1,
                                })
                            },
                            CircuitState::Open { failure_count, .. } => {
                                // Already open - extend timeout
                                Op::Put(CircuitState::Open {
                                    opened_at: Instant::now(),
                                    failure_count: failure_count.saturating_add(1),
                                })
                            },
                        }
                    },
                )
            });
    }

    /// Get the current state for a key.
    #[allow(dead_code)] // Inspection method for debugging/monitoring
    pub fn get_state(&self, key: &str) -> CircuitState {
        self.states.get(key).unwrap_or_default()
    }

    /// Get failure count for a key.
    #[allow(dead_code)] // Inspection method for debugging/monitoring
    pub fn failure_count(&self, key: &str) -> u32 {
        match self.states.get(key) {
            Some(
                CircuitState::Closed { failure_count } | CircuitState::Open { failure_count, .. },
            ) => failure_count,
            Some(CircuitState::HalfOpen { .. }) | None => 0,
        }
    }

    /// Reset circuit for a key.
    #[allow(dead_code)] // Inspection method for debugging/monitoring
    pub fn reset(&self, key: &str) {
        let cache_key: Arc<str> = Arc::from(key);
        self.states
            .entry_by_ref(&cache_key)
            .and_compute_with(|entry| {
                if entry.is_some() {
                    info!("Manually resetting circuit breaker for '{}'", key);
                    Op::Put(CircuitState::Closed { failure_count: 0 })
                } else {
                    Op::Nop
                }
            });
    }

    /// Get the number of tracked keys.
    #[allow(dead_code)] // Inspection method for debugging/monitoring
    pub fn tracked_count(&self) -> u64 {
        self.states.entry_count()
    }

    /// Get all tracked circuit states (for metrics/debugging).
    ///
    /// Returns a vector of `(key, state_name)` pairs where `state_name` is
    /// `closed`, `open`, or `half_open`.
    pub fn get_all_states(&self) -> Vec<(String, String)> {
        self.states.run_pending_tasks();
        self.states
            .iter()
            .map(|(k, v)| {
                let state_name = match v {
                    CircuitState::Closed { .. } => "closed",
                    CircuitState::Open { .. } => "open",
                    CircuitState::HalfOpen { .. } => "half_open",
                };
                (k.deref().to_string(), state_name.to_string())
            })
            .collect()
    }

    /// Get access to the internal states cache (for testing).
    #[cfg(test)]
    pub(crate) fn states(&self) -> &MokaCache<Arc<str>, CircuitState> {
        &self.states
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}
