// =============================================================================
// Lint Configuration
// =============================================================================

// Safety: deny unsafe by default, allow only where documented
// (wasmtime AOT cache in runtime/mod.rs, Unix setsid in daemon/process.rs)
#![deny(unsafe_code)]
// Correctness: Must handle all fallible operations
#![deny(unused_must_use)]
// Quality: Pedantic but pragmatic
#![warn(clippy::all)]
#![warn(clippy::pedantic)]
#![warn(rust_2018_idioms)]
#![warn(unreachable_pub)]
#![allow(missing_debug_implementations)] // Types contain wasmtime::Engine, MokaCache which lack Debug

// Allowed with documented reasons
#![allow(clippy::missing_errors_doc)] // Error returns self-documenting via type
#![allow(clippy::missing_panics_doc)] // Panics documented in main entry points
#![allow(clippy::module_name_repetitions)] // e.g., runtime::RuntimeConfig is clearer
#![allow(clippy::doc_markdown)] // Too many false positives in code docs
#![allow(clippy::must_use_candidate)] // Not all returned values need annotation
#![allow(clippy::cast_possible_truncation)] // Intentional in HTTP size calculations
#![allow(clippy::cast_sign_loss)] // Intentional in size calculations

//! Library crate for mik - exposes modules for testing and integration.
//!
//! This module exposes internal functionality for:
//! - Fuzz testing security-critical functions
//! - Integration testing with the real runtime
//!
//! # Security Functions
//!
//! The following security functions are exposed for fuzzing:
//!
//! - [`security::sanitize_file_path`] - Path traversal prevention
//! - [`security::sanitize_module_name`] - Module name validation
//! - [`security::validate_windows_path`] - Windows-specific path validation
//! - [`security::validate_path_within_base`] - Symlink escape prevention
//!
//! # Runtime
//!
//! The [`runtime`] module exposes the WASI HTTP host for integration testing:
//!
//! - [`runtime::HostBuilder`] - Builder for creating a Host instance
//! - [`runtime::Host`] - The WASI HTTP host that serves WASM components
//!
//! # Example
//!
//! ```
//! use mik::security::{sanitize_file_path, sanitize_module_name};
//!
//! // Valid paths are normalized
//! assert!(sanitize_file_path("assets/style.css").is_ok());
//!
//! // Path traversal is blocked
//! assert!(sanitize_file_path("../etc/passwd").is_err());
//!
//! // Valid module names pass
//! assert!(sanitize_module_name("my-module").is_ok());
//!
//! // Path separators in module names are blocked
//! assert!(sanitize_module_name("../evil").is_err());
//! ```

/// WASI HTTP runtime for serving WASM components.
///
/// This module provides the core runtime for loading and executing WASM
/// components with the WASI HTTP interface.
///
/// # Examples
///
/// ```no_run
/// use mik::runtime::HostBuilder;
/// use std::net::SocketAddr;
///
/// # async fn example() -> anyhow::Result<()> {
/// let host = HostBuilder::from_manifest("mik.toml")?
///     .build()?;
/// let addr: SocketAddr = "127.0.0.1:3000".parse()?;
/// host.serve(addr).await?;
/// # Ok(())
/// # }
/// ```
#[path = "runtime/mod.rs"]
pub mod runtime;

/// Security utilities for input sanitization and path traversal prevention.
///
/// This module provides functions to validate and sanitize untrusted input
/// to prevent security vulnerabilities like path traversal attacks.
///
/// # Examples
///
/// ```
/// use mik::security::{sanitize_file_path, sanitize_module_name};
///
/// // Validate user-provided paths
/// let path = sanitize_file_path("images/logo.png").unwrap();
///
/// // Validate module names from URL parameters
/// let module = sanitize_module_name("my-handler").unwrap();
/// ```
pub use runtime::security;

/// Reliability primitives (circuit breaker, rate limiting).
///
/// This module provides patterns for building resilient services:
/// - Circuit breaker for failing fast on unhealthy dependencies
/// - Rate limiting to prevent resource exhaustion
#[path = "reliability/mod.rs"]
pub mod reliability;

/// Centralized constants for security limits and defaults.
///
/// All magic numbers in the runtime should be defined here with
/// documented rationale. Includes limits for:
/// - Request body sizes
/// - Timeout durations
/// - Cache sizes
/// - Concurrency limits
pub mod constants;

/// Configuration types for the mik runtime.
///
/// This module provides configuration structs for:
/// - Server settings ([`config::ServerConfig`])
/// - Package metadata ([`config::Package`])
/// - Route definitions ([`config::RouteConfig`])
///
/// All configuration types support serde for TOML parsing and provide
/// sensible defaults suitable for development use.
///
/// # Example
///
/// ```
/// use mik::config::Config;
///
/// let toml = r#"
/// [package]
/// name = "my-service"
/// version = "0.1.0"
///
/// [server]
/// port = 3000
/// "#;
///
/// let config: Config = toml::from_str(toml).unwrap();
/// assert!(config.validate().is_ok());
/// ```
pub mod config;

/// Manifest types for mik.toml configuration.
///
/// This module provides types for parsing and representing
/// the mik.toml manifest file, including load balancer configuration.
pub mod manifest;
