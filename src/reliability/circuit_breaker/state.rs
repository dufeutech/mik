//! Circuit breaker state machine.
//!
//! Defines the three states a circuit breaker can be in:
//! - **Closed**: Normal operation, requests allowed
//! - **Open**: Too many failures, requests rejected
//! - **HalfOpen**: Testing recovery - only ONE probe request allowed

use std::time::Instant;

/// Circuit breaker state.
#[derive(Debug, Clone)]
pub enum CircuitState {
    /// Circuit is closed - requests allowed.
    Closed {
        /// Number of consecutive failures.
        failure_count: u32,
    },
    /// Circuit is open - requests blocked.
    Open {
        /// When the circuit was opened.
        opened_at: Instant,
        /// Number of failures that caused the circuit to open.
        failure_count: u32,
    },
    /// Circuit is half-open - testing recovery with a single probe.
    /// Only ONE request is allowed through; others are rejected.
    HalfOpen {
        /// When half-open started (for probe timeout).
        started_at: Instant,
    },
}

impl PartialEq for CircuitState {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (
                CircuitState::Closed { failure_count: a },
                CircuitState::Closed { failure_count: b }
            ) if a == b
        ) || matches!(
            (self, other),
            (CircuitState::HalfOpen { .. }, CircuitState::HalfOpen { .. })
        ) || matches!(
            (self, other),
            (
                CircuitState::Open { failure_count: a, .. },
                CircuitState::Open { failure_count: b, .. }
            ) if a == b
        )
    }
}

impl Eq for CircuitState {}

impl Default for CircuitState {
    fn default() -> Self {
        Self::Closed { failure_count: 0 }
    }
}
