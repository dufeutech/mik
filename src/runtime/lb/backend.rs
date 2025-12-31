//! Backend server representation and state management.
//!
//! Provides the [`Backend`] type which tracks the state of individual backend
//! servers including health status, active connections, and circuit breaker state.
//!
//! # Key Types
//!
//! - [`Backend`] - Represents a single backend server with health and connection tracking
//! - [`BackendState`] - Enum representing backend health states (Healthy, Unhealthy, etc.)
//!
//! Each backend tracks metrics like active requests, failure counts, and supports
//! optional circuit breaker integration for fault isolation.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::RwLock;

use super::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig};

/// State of a backend server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum BackendState {
    /// Backend is healthy and accepting requests.
    Healthy,
    /// Backend is unhealthy and should not receive requests.
    Unhealthy,
    /// Backend health is unknown (initial state).
    Unknown,
    /// Backend is being drained (no new requests, completing existing ones).
    Draining,
}

impl fmt::Display for BackendState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Healthy => "healthy",
            Self::Unhealthy => "unhealthy",
            Self::Unknown => "unknown",
            Self::Draining => "draining",
        };
        write!(f, "{s}")
    }
}

/// Default maximum concurrent connections per backend.
pub(super) const DEFAULT_MAX_CONNECTIONS: u32 = 100;

/// A backend server that can receive proxied requests.
#[derive(Debug)]
pub struct Backend {
    /// Address of the backend (e.g., "127.0.0.1:3001").
    address: String,
    /// Weight for weighted load balancing (higher = more traffic).
    /// Default is 1. A backend with weight 2 receives 2x traffic vs weight 1.
    weight: u32,
    /// Maximum concurrent connections allowed for this backend.
    /// When `active_requests` >= `max_connections`, the backend is at capacity.
    max_connections: u32,
    /// Whether the backend is currently healthy.
    healthy: AtomicBool,
    /// Number of consecutive health check failures.
    failure_count: AtomicU64,
    /// Number of consecutive health check successes.
    success_count: AtomicU64,
    /// Total requests handled.
    total_requests: AtomicU64,
    /// Currently active requests.
    active_requests: AtomicU64,
    /// Last health check time.
    last_check: RwLock<Option<Instant>>,
    /// Last successful response time.
    last_success: RwLock<Option<Instant>>,
    /// Optional circuit breaker for this backend.
    circuit_breaker: Option<CircuitBreaker>,
}

impl Backend {
    /// Create a new backend with the given address, default weight of 1,
    /// and default max connections.
    pub fn new(address: String) -> Self {
        Self::with_options(address, 1, DEFAULT_MAX_CONNECTIONS)
    }

    /// Create a new backend with the given address and weight.
    ///
    /// Weight determines the proportion of traffic this backend receives.
    /// A backend with weight 2 will receive twice as much traffic as one with weight 1.
    /// Weight of 0 is treated as 1 (minimum weight).
    #[allow(dead_code)]
    pub fn with_weight(address: String, weight: u32) -> Self {
        Self::with_options(address, weight, DEFAULT_MAX_CONNECTIONS)
    }

    /// Create a new backend with the given address, weight, and max connections.
    ///
    /// Weight determines the proportion of traffic this backend receives.
    /// A backend with weight 2 will receive twice as much traffic as one with weight 1.
    /// Weight of 0 is treated as 1 (minimum weight).
    ///
    /// `max_connections` limits the number of concurrent connections to this backend.
    /// When the limit is reached, the backend is considered at capacity and will
    /// not be selected for new requests until active connections decrease.
    pub fn with_options(address: String, weight: u32, max_connections: u32) -> Self {
        Self {
            address,
            weight: weight.max(1), // Ensure minimum weight of 1
            max_connections: max_connections.max(1), // Ensure at least 1 connection allowed
            healthy: AtomicBool::new(true), // Assume healthy until proven otherwise
            failure_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            total_requests: AtomicU64::new(0),
            active_requests: AtomicU64::new(0),
            last_check: RwLock::new(None),
            last_success: RwLock::new(None),
            circuit_breaker: None,
        }
    }

    /// Create a new backend with a circuit breaker.
    ///
    /// The circuit breaker will automatically open after consecutive failures
    /// and close after successful requests in the half-open state.
    #[allow(dead_code)]
    pub fn with_circuit_breaker(address: String, config: CircuitBreakerConfig) -> Self {
        Self {
            address,
            weight: 1,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            healthy: AtomicBool::new(true),
            failure_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            total_requests: AtomicU64::new(0),
            active_requests: AtomicU64::new(0),
            last_check: RwLock::new(None),
            last_success: RwLock::new(None),
            circuit_breaker: Some(CircuitBreaker::new(config)),
        }
    }

    /// Create a new backend with all options including circuit breaker.
    #[allow(dead_code)]
    pub fn with_all_options(
        address: String,
        weight: u32,
        max_connections: u32,
        circuit_breaker_config: Option<CircuitBreakerConfig>,
    ) -> Self {
        Self {
            address,
            weight: weight.max(1),
            max_connections: max_connections.max(1),
            healthy: AtomicBool::new(true),
            failure_count: AtomicU64::new(0),
            success_count: AtomicU64::new(0),
            total_requests: AtomicU64::new(0),
            active_requests: AtomicU64::new(0),
            last_check: RwLock::new(None),
            last_success: RwLock::new(None),
            circuit_breaker: circuit_breaker_config.map(CircuitBreaker::new),
        }
    }

    /// Get the backend address.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Get the backend weight for load balancing.
    #[allow(dead_code)]
    pub const fn weight(&self) -> u32 {
        self.weight
    }

    /// Get the maximum connections limit for this backend.
    #[allow(dead_code)]
    pub const fn max_connections(&self) -> u32 {
        self.max_connections
    }

    /// Check if the backend has capacity for more connections.
    ///
    /// Returns true if `active_requests` < `max_connections`, meaning
    /// the backend can accept new requests.
    pub fn has_capacity(&self) -> bool {
        self.active_requests.load(Ordering::Acquire) < u64::from(self.max_connections)
    }

    /// Get the full URL for a given path.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.address, path)
    }

    /// Check if the backend is healthy.
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    /// Get the current state of the backend.
    #[allow(dead_code)]
    pub fn state(&self) -> BackendState {
        if self.last_check.read().is_none() {
            BackendState::Unknown
        } else if self.is_healthy() {
            BackendState::Healthy
        } else {
            BackendState::Unhealthy
        }
    }

    /// Mark the backend as healthy after a successful health check.
    pub fn mark_healthy(&self) {
        self.healthy.store(true, Ordering::Release);
        self.failure_count.store(0, Ordering::Release);
        self.success_count.fetch_add(1, Ordering::AcqRel);
        *self.last_check.write() = Some(Instant::now());
        *self.last_success.write() = Some(Instant::now());
    }

    /// Mark the backend as unhealthy after a failed health check.
    pub fn mark_unhealthy(&self) {
        self.healthy.store(false, Ordering::Release);
        self.success_count.store(0, Ordering::Release);
        self.failure_count.fetch_add(1, Ordering::AcqRel);
        *self.last_check.write() = Some(Instant::now());
    }

    /// Record a successful request.
    ///
    /// This updates the total request count and also notifies the circuit breaker
    /// (if configured) about the successful request.
    pub fn record_success(&self) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        *self.last_success.write() = Some(Instant::now());

        // Update circuit breaker if present
        if let Some(ref cb) = self.circuit_breaker {
            cb.record_success();
        }
    }

    /// Record a failed request.
    ///
    /// This updates the total request count and also notifies the circuit breaker
    /// (if configured) about the failed request.
    pub fn record_failure(&self) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        // Don't mark unhealthy on request failure - that's the health check's job

        // Update circuit breaker if present
        if let Some(ref cb) = self.circuit_breaker {
            cb.record_failure();
        }
    }

    /// Check if the backend is available for requests.
    ///
    /// Returns true if:
    /// - The backend is healthy AND
    /// - The circuit breaker allows requests (if configured)
    ///
    /// This combines health check status with circuit breaker state.
    pub fn is_available(&self) -> bool {
        // Check health first
        if !self.is_healthy() {
            return false;
        }

        // Check circuit breaker if present
        if let Some(ref cb) = self.circuit_breaker {
            return cb.is_available();
        }

        true
    }

    /// Get a reference to the circuit breaker (if configured).
    #[allow(dead_code)]
    pub const fn circuit_breaker(&self) -> Option<&CircuitBreaker> {
        self.circuit_breaker.as_ref()
    }

    /// Increment active request count.
    pub fn start_request(&self) {
        self.active_requests.fetch_add(1, Ordering::AcqRel);
    }

    /// Decrement active request count.
    pub fn end_request(&self) {
        self.active_requests.fetch_sub(1, Ordering::AcqRel);
    }

    /// Get the number of active requests.
    #[allow(dead_code)]
    pub fn active_requests(&self) -> u64 {
        self.active_requests.load(Ordering::Acquire)
    }

    /// Get the total number of requests handled.
    #[allow(dead_code)]
    pub fn total_requests(&self) -> u64 {
        self.total_requests.load(Ordering::Relaxed)
    }

    /// Get the consecutive failure count.
    pub fn failure_count(&self) -> u64 {
        self.failure_count.load(Ordering::Relaxed)
    }

    /// Get the consecutive success count.
    pub fn success_count(&self) -> u64 {
        self.success_count.load(Ordering::Relaxed)
    }
}

impl Clone for Backend {
    fn clone(&self) -> Self {
        Self {
            address: self.address.clone(),
            weight: self.weight,
            max_connections: self.max_connections,
            healthy: AtomicBool::new(self.healthy.load(Ordering::Acquire)),
            failure_count: AtomicU64::new(self.failure_count.load(Ordering::Relaxed)),
            success_count: AtomicU64::new(self.success_count.load(Ordering::Relaxed)),
            total_requests: AtomicU64::new(self.total_requests.load(Ordering::Relaxed)),
            active_requests: AtomicU64::new(self.active_requests.load(Ordering::Relaxed)),
            last_check: RwLock::new(*self.last_check.read()),
            last_success: RwLock::new(*self.last_success.read()),
            circuit_breaker: self.circuit_breaker.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::circuit_breaker::CircuitBreakerState;
    use super::*;

    #[test]
    fn test_backend_new() {
        let backend = Backend::new("127.0.0.1:3001".to_string());
        assert_eq!(backend.address(), "127.0.0.1:3001");
        assert!(backend.is_healthy()); // Assume healthy by default
        assert_eq!(backend.state(), BackendState::Unknown); // No health check yet
    }

    #[test]
    fn test_backend_url() {
        let backend = Backend::new("127.0.0.1:3001".to_string());
        assert_eq!(backend.url("/health"), "http://127.0.0.1:3001/health");
        assert_eq!(backend.url("/run/echo/"), "http://127.0.0.1:3001/run/echo/");
    }

    #[test]
    fn test_backend_health_transitions() {
        let backend = Backend::new("127.0.0.1:3001".to_string());

        // Initial state
        assert!(backend.is_healthy());
        assert_eq!(backend.failure_count(), 0);
        assert_eq!(backend.success_count(), 0);

        // Mark healthy
        backend.mark_healthy();
        assert!(backend.is_healthy());
        assert_eq!(backend.state(), BackendState::Healthy);
        assert_eq!(backend.success_count(), 1);
        assert_eq!(backend.failure_count(), 0);

        // Mark unhealthy
        backend.mark_unhealthy();
        assert!(!backend.is_healthy());
        assert_eq!(backend.state(), BackendState::Unhealthy);
        assert_eq!(backend.failure_count(), 1);
        assert_eq!(backend.success_count(), 0); // Reset on failure

        // Mark healthy again
        backend.mark_healthy();
        assert!(backend.is_healthy());
        assert_eq!(backend.failure_count(), 0); // Reset on success
    }

    #[test]
    fn test_backend_request_tracking() {
        let backend = Backend::new("127.0.0.1:3001".to_string());

        assert_eq!(backend.active_requests(), 0);
        assert_eq!(backend.total_requests(), 0);

        backend.start_request();
        assert_eq!(backend.active_requests(), 1);

        backend.start_request();
        assert_eq!(backend.active_requests(), 2);

        backend.record_success();
        assert_eq!(backend.total_requests(), 1);

        backend.end_request();
        assert_eq!(backend.active_requests(), 1);

        backend.end_request();
        assert_eq!(backend.active_requests(), 0);
    }

    #[test]
    fn test_backend_weight() {
        // Default weight is 1
        let backend = Backend::new("127.0.0.1:3001".to_string());
        assert_eq!(backend.weight(), 1);

        // Custom weight
        let backend = Backend::with_weight("127.0.0.1:3001".to_string(), 5);
        assert_eq!(backend.weight(), 5);

        // Weight of 0 should be treated as 1
        let backend = Backend::with_weight("127.0.0.1:3001".to_string(), 0);
        assert_eq!(backend.weight(), 1);
    }

    #[test]
    fn test_backend_max_connections() {
        // Default max connections
        let backend = Backend::new("127.0.0.1:3001".to_string());
        assert_eq!(backend.max_connections(), DEFAULT_MAX_CONNECTIONS);
        assert!(backend.has_capacity());

        // Custom max connections
        let backend = Backend::with_options("127.0.0.1:3001".to_string(), 1, 2);
        assert_eq!(backend.max_connections(), 2);
        assert!(backend.has_capacity());

        // Simulate reaching capacity
        backend.start_request();
        assert!(backend.has_capacity()); // 1 < 2
        backend.start_request();
        assert!(!backend.has_capacity()); // 2 >= 2

        backend.end_request();
        assert!(backend.has_capacity()); // 1 < 2
    }

    #[test]
    fn test_backend_clone() {
        let backend = Backend::with_options("127.0.0.1:3001".to_string(), 3, 50);
        backend.mark_healthy();
        backend.start_request();

        let cloned = backend.clone();
        assert_eq!(cloned.address(), backend.address());
        assert_eq!(cloned.weight(), backend.weight());
        assert_eq!(cloned.max_connections(), backend.max_connections());
        assert_eq!(cloned.is_healthy(), backend.is_healthy());
        assert_eq!(cloned.active_requests(), backend.active_requests());
    }

    // ============ Circuit Breaker Integration Tests ============

    #[test]
    fn test_backend_with_circuit_breaker() {
        use std::time::Duration;

        let config = CircuitBreakerConfig::new(3, 2, Duration::from_secs(30));
        let backend = Backend::with_circuit_breaker("127.0.0.1:3001".to_string(), config);

        assert!(backend.circuit_breaker().is_some());
        assert!(backend.is_available());

        let cb = backend.circuit_breaker().unwrap();
        assert_eq!(cb.state(), CircuitBreakerState::Closed);
    }

    #[test]
    fn test_backend_without_circuit_breaker() {
        let backend = Backend::new("127.0.0.1:3001".to_string());

        assert!(backend.circuit_breaker().is_none());
        assert!(backend.is_available());
    }

    #[test]
    fn test_backend_circuit_breaker_opens_on_failures() {
        use std::time::Duration;

        // Circuit breaker opens after 3 failures
        let config = CircuitBreakerConfig::new(3, 2, Duration::from_secs(30));
        let backend = Backend::with_circuit_breaker("127.0.0.1:3001".to_string(), config);

        assert!(backend.is_available());

        // Record failures
        backend.record_failure();
        assert!(backend.is_available());

        backend.record_failure();
        assert!(backend.is_available());

        backend.record_failure();
        // Circuit should now be open
        assert!(!backend.is_available());

        let cb = backend.circuit_breaker().unwrap();
        assert_eq!(cb.state(), CircuitBreakerState::Open);
    }

    #[test]
    fn test_backend_circuit_breaker_half_open_after_timeout() {
        use std::time::Duration;

        // Very short timeout for testing
        let config = CircuitBreakerConfig::new(1, 1, Duration::from_millis(10));
        let backend = Backend::with_circuit_breaker("127.0.0.1:3001".to_string(), config);

        // Open the circuit
        backend.record_failure();
        assert!(!backend.is_available());

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(15));

        // Should now be half-open and available
        assert!(backend.is_available());

        let cb = backend.circuit_breaker().unwrap();
        assert_eq!(cb.state(), CircuitBreakerState::HalfOpen);
    }

    #[test]
    fn test_backend_circuit_breaker_closes_on_success() {
        use std::time::Duration;

        // Needs 1 success in half-open to close
        let config = CircuitBreakerConfig::new(1, 1, Duration::from_millis(10));
        let backend = Backend::with_circuit_breaker("127.0.0.1:3001".to_string(), config);

        // Open the circuit
        backend.record_failure();
        assert!(!backend.is_available());

        // Wait for half-open
        std::thread::sleep(Duration::from_millis(15));
        assert!(backend.is_available());

        // Record success
        backend.record_success();

        // Should be closed now
        let cb = backend.circuit_breaker().unwrap();
        assert_eq!(cb.state(), CircuitBreakerState::Closed);
        assert!(backend.is_available());
    }

    #[test]
    fn test_backend_is_available_combines_health_and_circuit_breaker() {
        use std::time::Duration;

        let config = CircuitBreakerConfig::new(1, 1, Duration::from_secs(30));
        let backend = Backend::with_circuit_breaker("127.0.0.1:3001".to_string(), config);

        // Initially healthy and circuit closed
        assert!(backend.is_healthy());
        assert!(backend.is_available());

        // Mark unhealthy - should be unavailable
        backend.mark_unhealthy();
        assert!(!backend.is_healthy());
        assert!(!backend.is_available());

        // Mark healthy again - circuit still closed, should be available
        backend.mark_healthy();
        assert!(backend.is_healthy());
        assert!(backend.is_available());

        // Open the circuit - healthy but circuit open, should be unavailable
        backend.record_failure();
        assert!(backend.is_healthy());
        assert!(!backend.is_available());
    }

    #[test]
    fn test_backend_with_all_options_circuit_breaker() {
        use std::time::Duration;

        // With circuit breaker
        let config = CircuitBreakerConfig::new(5, 2, Duration::from_secs(60));
        let backend = Backend::with_all_options("127.0.0.1:3001".to_string(), 2, 50, Some(config));

        assert_eq!(backend.weight(), 2);
        assert_eq!(backend.max_connections(), 50);
        assert!(backend.circuit_breaker().is_some());

        // Without circuit breaker
        let backend = Backend::with_all_options("127.0.0.1:3001".to_string(), 3, 100, None);

        assert_eq!(backend.weight(), 3);
        assert_eq!(backend.max_connections(), 100);
        assert!(backend.circuit_breaker().is_none());
    }

    #[test]
    fn test_backend_clone_with_circuit_breaker() {
        use std::time::Duration;

        let config = CircuitBreakerConfig::new(3, 2, Duration::from_secs(30));
        let backend = Backend::with_circuit_breaker("127.0.0.1:3001".to_string(), config);

        // Record a failure
        backend.record_failure();

        // Clone the backend
        let cloned = backend.clone();

        assert!(cloned.circuit_breaker().is_some());
        assert_eq!(
            cloned.circuit_breaker().unwrap().failure_count(),
            backend.circuit_breaker().unwrap().failure_count()
        );
    }
}
