//! Production-ready reliability primitives built on battle-tested crates.
//!
//! This module provides:
//!
//! - **Circuit Breaker** - Per-key with LRU eviction and half-open recovery
//! - **Retry** - Backoff strategies via [backon](https://docs.rs/backon)
//! - **Rate Limiting** - Token bucket via [governor](https://docs.rs/governor)
//!
//! ## Circuit Breaker (Check/Record Pattern)
//!
//! Best for request handlers where you need to check before and record after.
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

mod circuit_breaker;
pub mod security;

// Re-export circuit breaker types
// Note: CircuitBreakerConfig and CircuitState are used by tests and external callers
#[allow(unused_imports)]
pub use circuit_breaker::{
    CircuitBreaker, CircuitBreakerConfig, CircuitOpenError, CircuitOpenReason, CircuitState,
};
pub use security::is_http_host_allowed;
