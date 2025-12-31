//! Circuit breaker implementation for backend servers.
//!
//! The circuit breaker pattern prevents cascading failures by temporarily
//! stopping requests to a failing backend. It has three states:
//!
//! - **Closed**: Normal operation, requests flow through
//! - **Open**: Backend is failing, requests are blocked
//! - **`HalfOpen`**: Testing if backend has recovered
//!
//! # State Transitions
//!
//! ```text
//! ┌────────────────────────────────────────────────────────────┐
//! │                                                            │
//! │  ┌──────────┐    failures >= threshold    ┌──────────┐    │
//! │  │  Closed  │ ────────────────────────────▶  Open    │    │
//! │  └──────────┘                              └──────────┘    │
//! │       ▲                                         │          │
//! │       │                                         │          │
//! │       │ successes >= threshold            timeout elapsed  │
//! │       │                                         │          │
//! │       │         ┌────────────┐                  │          │
//! │       └─────────│  HalfOpen  │◀─────────────────┘          │
//! │                 └────────────┘                             │
//! │                       │                                    │
//! │                       │ failure                            │
//! │                       ▼                                    │
//! │                   ┌──────────┐                             │
//! │                   │   Open   │ (reset timer)               │
//! │                   └──────────┘                             │
//! └────────────────────────────────────────────────────────────┘
//! ```

use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use parking_lot::RwLock;

use crate::constants;

/// Default number of failures before opening the circuit.
pub(crate) const DEFAULT_FAILURE_THRESHOLD: u32 = constants::CIRCUIT_BREAKER_FAILURE_THRESHOLD;

/// Default number of successes in half-open state to close the circuit.
pub(crate) const DEFAULT_SUCCESS_THRESHOLD: u32 = constants::CIRCUIT_BREAKER_SUCCESS_THRESHOLD;

/// Default timeout before transitioning from open to half-open.
pub(crate) const DEFAULT_TIMEOUT: Duration =
    Duration::from_secs(constants::CIRCUIT_BREAKER_TIMEOUT_SECS);

/// Configuration for the circuit breaker.
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of consecutive failures before opening the circuit.
    pub failure_threshold: u32,
    /// Number of consecutive successes in half-open state to close the circuit.
    pub success_threshold: u32,
    /// How long to stay open before transitioning to half-open.
    pub timeout: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: DEFAULT_FAILURE_THRESHOLD,
            success_threshold: DEFAULT_SUCCESS_THRESHOLD,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl CircuitBreakerConfig {
    /// Create a new circuit breaker configuration.
    #[allow(dead_code)]
    pub fn new(failure_threshold: u32, success_threshold: u32, timeout: Duration) -> Self {
        Self {
            failure_threshold: failure_threshold.max(1),
            success_threshold: success_threshold.max(1),
            timeout,
        }
    }
}

/// State of the circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CircuitBreakerState {
    /// Normal operation, requests flow through.
    Closed = 0,
    /// Backend is failing, requests are blocked.
    Open = 1,
    /// Testing if backend has recovered.
    HalfOpen = 2,
}

impl From<u8> for CircuitBreakerState {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Open,
            2 => Self::HalfOpen,
            _ => Self::Closed,
        }
    }
}

/// Circuit breaker for a single backend.
///
/// Provides thread-safe state management using atomic operations.
#[derive(Debug)]
pub struct CircuitBreaker {
    /// Configuration for the circuit breaker.
    config: CircuitBreakerConfig,
    /// Current state (stored as u8 for atomic operations).
    state: AtomicU8,
    /// Consecutive failure count.
    failure_count: AtomicU32,
    /// Consecutive success count (used in half-open state).
    success_count: AtomicU32,
    /// When the circuit was opened (for timeout calculation).
    opened_at: RwLock<Option<Instant>>,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with the given configuration.
    pub const fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            state: AtomicU8::new(CircuitBreakerState::Closed as u8),
            failure_count: AtomicU32::new(0),
            success_count: AtomicU32::new(0),
            opened_at: RwLock::new(None),
        }
    }

    /// Create a new circuit breaker with default configuration.
    #[allow(dead_code)]
    pub fn with_defaults() -> Self {
        Self::new(CircuitBreakerConfig::default())
    }

    /// Get the current state of the circuit breaker.
    pub fn state(&self) -> CircuitBreakerState {
        // Check if we should transition from Open to HalfOpen
        let current_state = CircuitBreakerState::from(self.state.load(Ordering::Acquire));

        if current_state == CircuitBreakerState::Open
            && let Some(opened_at) = *self.opened_at.read()
            && opened_at.elapsed() >= self.config.timeout
        {
            // Transition to HalfOpen
            self.transition_to_half_open();
            return CircuitBreakerState::HalfOpen;
        }

        current_state
    }

    /// Check if the circuit breaker allows requests.
    ///
    /// Returns `true` if:
    /// - State is Closed (normal operation)
    /// - State is `HalfOpen` (testing recovery)
    ///
    /// Returns `false` if:
    /// - State is Open (blocking requests)
    pub fn is_available(&self) -> bool {
        self.state() != CircuitBreakerState::Open
    }

    /// Record a successful request.
    ///
    /// In Closed state: resets failure count.
    /// In `HalfOpen` state: increments success count, may transition to Closed.
    /// In Open state: ignored (shouldn't happen if `is_available` is checked).
    pub fn record_success(&self) {
        let current_state = self.state();

        match current_state {
            CircuitBreakerState::Closed => {
                // Reset failure count on success
                self.failure_count.store(0, Ordering::Release);
            },
            CircuitBreakerState::HalfOpen => {
                // Increment success count
                let successes = self.success_count.fetch_add(1, Ordering::AcqRel) + 1;

                // Check if we should close the circuit
                if successes >= self.config.success_threshold {
                    self.transition_to_closed();
                }
            },
            CircuitBreakerState::Open => {
                // Shouldn't happen, but ignore
            },
        }
    }

    /// Record a failed request.
    ///
    /// In Closed state: increments failure count, may transition to Open.
    /// In `HalfOpen` state: immediately transitions to Open.
    /// In Open state: ignored (shouldn't happen if `is_available` is checked).
    pub fn record_failure(&self) {
        let current_state = self.state();

        match current_state {
            CircuitBreakerState::Closed => {
                // Increment failure count
                let failures = self.failure_count.fetch_add(1, Ordering::AcqRel) + 1;

                // Check if we should open the circuit
                if failures >= self.config.failure_threshold {
                    self.transition_to_open();
                }
            },
            CircuitBreakerState::HalfOpen => {
                // Any failure in half-open immediately opens the circuit
                self.transition_to_open();
            },
            CircuitBreakerState::Open => {
                // Shouldn't happen, but ignore
            },
        }
    }

    /// Get the current failure count.
    #[allow(dead_code)]
    pub fn failure_count(&self) -> u32 {
        self.failure_count.load(Ordering::Relaxed)
    }

    /// Get the current success count (meaningful in half-open state).
    #[allow(dead_code)]
    pub fn success_count(&self) -> u32 {
        self.success_count.load(Ordering::Relaxed)
    }

    /// Get the configuration.
    #[allow(dead_code)]
    pub const fn config(&self) -> &CircuitBreakerConfig {
        &self.config
    }

    /// Transition to Open state.
    fn transition_to_open(&self) {
        self.state
            .store(CircuitBreakerState::Open as u8, Ordering::Release);
        self.success_count.store(0, Ordering::Release);
        *self.opened_at.write() = Some(Instant::now());
    }

    /// Transition to `HalfOpen` state.
    fn transition_to_half_open(&self) {
        self.state
            .store(CircuitBreakerState::HalfOpen as u8, Ordering::Release);
        self.success_count.store(0, Ordering::Release);
        self.failure_count.store(0, Ordering::Release);
    }

    /// Transition to Closed state.
    fn transition_to_closed(&self) {
        self.state
            .store(CircuitBreakerState::Closed as u8, Ordering::Release);
        self.failure_count.store(0, Ordering::Release);
        self.success_count.store(0, Ordering::Release);
        *self.opened_at.write() = None;
    }

    /// Reset the circuit breaker to its initial state.
    #[allow(dead_code)]
    pub fn reset(&self) {
        self.transition_to_closed();
    }
}

impl Clone for CircuitBreaker {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            state: AtomicU8::new(self.state.load(Ordering::Acquire)),
            failure_count: AtomicU32::new(self.failure_count.load(Ordering::Relaxed)),
            success_count: AtomicU32::new(self.success_count.load(Ordering::Relaxed)),
            opened_at: RwLock::new(*self.opened_at.read()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_config_default() {
        let config = CircuitBreakerConfig::default();
        assert_eq!(config.failure_threshold, DEFAULT_FAILURE_THRESHOLD);
        assert_eq!(config.success_threshold, DEFAULT_SUCCESS_THRESHOLD);
        assert_eq!(config.timeout, DEFAULT_TIMEOUT);
    }

    #[test]
    fn test_circuit_breaker_config_custom() {
        let config = CircuitBreakerConfig::new(3, 1, Duration::from_secs(10));
        assert_eq!(config.failure_threshold, 3);
        assert_eq!(config.success_threshold, 1);
        assert_eq!(config.timeout, Duration::from_secs(10));
    }

    #[test]
    fn test_circuit_breaker_config_minimum_values() {
        // Ensure minimum values are enforced
        let config = CircuitBreakerConfig::new(0, 0, Duration::from_secs(1));
        assert_eq!(config.failure_threshold, 1);
        assert_eq!(config.success_threshold, 1);
    }

    #[test]
    fn test_circuit_breaker_initial_state() {
        let cb = CircuitBreaker::with_defaults();
        assert_eq!(cb.state(), CircuitBreakerState::Closed);
        assert!(cb.is_available());
        assert_eq!(cb.failure_count(), 0);
        assert_eq!(cb.success_count(), 0);
    }

    #[test]
    fn test_circuit_breaker_opens_after_failures() {
        let config = CircuitBreakerConfig::new(3, 2, Duration::from_secs(30));
        let cb = CircuitBreaker::new(config);

        assert_eq!(cb.state(), CircuitBreakerState::Closed);
        assert!(cb.is_available());

        // First two failures - circuit should remain closed
        cb.record_failure();
        assert_eq!(cb.state(), CircuitBreakerState::Closed);
        assert!(cb.is_available());
        assert_eq!(cb.failure_count(), 1);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitBreakerState::Closed);
        assert!(cb.is_available());
        assert_eq!(cb.failure_count(), 2);

        // Third failure - circuit should open
        cb.record_failure();
        assert_eq!(cb.state(), CircuitBreakerState::Open);
        assert!(!cb.is_available());
    }

    #[test]
    fn test_circuit_breaker_success_resets_failure_count() {
        let config = CircuitBreakerConfig::new(3, 2, Duration::from_secs(30));
        let cb = CircuitBreaker::new(config);

        // Record some failures
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.failure_count(), 2);

        // Record a success - should reset failures
        cb.record_success();
        assert_eq!(cb.failure_count(), 0);
        assert_eq!(cb.state(), CircuitBreakerState::Closed);
    }

    #[test]
    fn test_circuit_breaker_half_open_after_timeout() {
        let config = CircuitBreakerConfig::new(1, 2, Duration::from_millis(10));
        let cb = CircuitBreaker::new(config);

        // Open the circuit
        cb.record_failure();
        assert_eq!(cb.state(), CircuitBreakerState::Open);
        assert!(!cb.is_available());

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(15));

        // Should transition to half-open
        assert_eq!(cb.state(), CircuitBreakerState::HalfOpen);
        assert!(cb.is_available());
    }

    #[test]
    fn test_circuit_breaker_closes_after_successes_in_half_open() {
        let config = CircuitBreakerConfig::new(1, 2, Duration::from_millis(10));
        let cb = CircuitBreaker::new(config);

        // Open the circuit
        cb.record_failure();
        assert_eq!(cb.state(), CircuitBreakerState::Open);

        // Wait for timeout to transition to half-open
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(cb.state(), CircuitBreakerState::HalfOpen);

        // First success - still half-open
        cb.record_success();
        assert_eq!(cb.state(), CircuitBreakerState::HalfOpen);
        assert_eq!(cb.success_count(), 1);

        // Second success - should close
        cb.record_success();
        assert_eq!(cb.state(), CircuitBreakerState::Closed);
        assert!(cb.is_available());
    }

    #[test]
    fn test_circuit_breaker_reopens_on_failure_in_half_open() {
        let config = CircuitBreakerConfig::new(1, 2, Duration::from_millis(10));
        let cb = CircuitBreaker::new(config);

        // Open the circuit
        cb.record_failure();
        assert_eq!(cb.state(), CircuitBreakerState::Open);

        // Wait for timeout to transition to half-open
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(cb.state(), CircuitBreakerState::HalfOpen);

        // Failure in half-open - should reopen immediately
        cb.record_failure();
        assert_eq!(cb.state(), CircuitBreakerState::Open);
        assert!(!cb.is_available());
    }

    #[test]
    fn test_circuit_breaker_reset() {
        let config = CircuitBreakerConfig::new(1, 2, Duration::from_secs(30));
        let cb = CircuitBreaker::new(config);

        // Open the circuit
        cb.record_failure();
        assert_eq!(cb.state(), CircuitBreakerState::Open);

        // Reset
        cb.reset();
        assert_eq!(cb.state(), CircuitBreakerState::Closed);
        assert!(cb.is_available());
        assert_eq!(cb.failure_count(), 0);
        assert_eq!(cb.success_count(), 0);
    }

    #[test]
    fn test_circuit_breaker_clone() {
        let config = CircuitBreakerConfig::new(3, 2, Duration::from_secs(30));
        let cb = CircuitBreaker::new(config);

        // Record some activity
        cb.record_failure();
        cb.record_failure();

        // Clone
        let cloned = cb.clone();
        assert_eq!(cloned.state(), cb.state());
        assert_eq!(cloned.failure_count(), cb.failure_count());
        assert_eq!(
            cloned.config().failure_threshold,
            cb.config().failure_threshold
        );
    }

    #[test]
    fn test_circuit_breaker_state_transitions() {
        let config = CircuitBreakerConfig::new(2, 1, Duration::from_millis(10));
        let cb = CircuitBreaker::new(config);

        // Closed -> Open
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitBreakerState::Open);

        // Open -> HalfOpen (after timeout)
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(cb.state(), CircuitBreakerState::HalfOpen);

        // HalfOpen -> Closed (after success)
        cb.record_success();
        assert_eq!(cb.state(), CircuitBreakerState::Closed);

        // Back to Open
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitBreakerState::Open);

        // Open -> HalfOpen -> Open (failure in half-open)
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(cb.state(), CircuitBreakerState::HalfOpen);
        cb.record_failure();
        assert_eq!(cb.state(), CircuitBreakerState::Open);
    }

    #[test]
    fn test_circuit_breaker_state_from_u8() {
        assert_eq!(CircuitBreakerState::from(0), CircuitBreakerState::Closed);
        assert_eq!(CircuitBreakerState::from(1), CircuitBreakerState::Open);
        assert_eq!(CircuitBreakerState::from(2), CircuitBreakerState::HalfOpen);
        // Invalid values default to Closed
        assert_eq!(CircuitBreakerState::from(255), CircuitBreakerState::Closed);
    }
}
