//! Re-exports from local reliability module.
//!
//! This module re-exports the reliability primitives used by the runtime,
//! providing access to circuit breaker patterns and security utilities
//! from the top-level `crate::reliability` module.
//!
//! # Re-exported Types
//!
//! - [`CircuitBreaker`] - Fault isolation pattern to prevent cascading failures
//! - [`is_http_host_allowed`] - Security utility for validating HTTP hosts

// Re-export circuit breaker
pub use crate::reliability::CircuitBreaker;

// Re-export security utilities
pub use crate::reliability::security::is_http_host_allowed;
