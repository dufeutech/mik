//! Circuit breaker error types.
//!
//! Defines error types returned when the circuit breaker rejects requests.

/// Error returned when circuit breaker rejects a request.
#[derive(Debug, Clone)]
pub struct CircuitOpenError {
    pub key: String,
    pub failure_count: u32,
    pub reason: CircuitOpenReason,
}

/// Reason why the circuit breaker rejected a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitOpenReason {
    /// Circuit is open due to failures
    Open,
    /// Circuit is half-open with a probe already in flight
    ProbeInFlight,
}

impl std::fmt::Display for CircuitOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.reason {
            CircuitOpenReason::Open => {
                write!(
                    f,
                    "Circuit breaker open for '{}' (failures: {})",
                    self.key, self.failure_count
                )
            },
            CircuitOpenReason::ProbeInFlight => {
                write!(
                    f,
                    "Circuit breaker for '{}' is testing recovery (probe in flight)",
                    self.key
                )
            },
        }
    }
}

impl std::error::Error for CircuitOpenError {}
