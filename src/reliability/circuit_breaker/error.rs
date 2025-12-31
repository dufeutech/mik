//! Circuit breaker error types.
//!
//! Defines error types returned when the circuit breaker rejects requests.

use thiserror::Error;

/// Reason why the circuit breaker rejected a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitOpenReason {
    /// Circuit is open due to failures
    Open,
    /// Circuit is half-open with a probe already in flight
    ProbeInFlight,
}

/// Error returned when circuit breaker rejects a request.
#[derive(Debug, Clone, Error)]
#[error("{}", match .reason {
    CircuitOpenReason::Open => format!("Circuit breaker open for '{}' (failures: {})", .key, .failure_count),
    CircuitOpenReason::ProbeInFlight => format!("Circuit breaker for '{}' is testing recovery (probe in flight)", .key),
})]
pub struct CircuitOpenError {
    pub key: String,
    pub failure_count: u32,
    pub reason: CircuitOpenReason,
}
